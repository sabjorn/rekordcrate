// Copyright (c) 2026 Jan Holthuis <jan.holthuis@rub.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy
// of the MPL was not distributed with this file, You can obtain one at
// http://mozilla.org/MPL/2.0/.
//
// SPDX-License-Identifier: MPL-2.0

//! Writing a Rekordbox device export: the databases, the settings file and the rows that
//! describe the tracks and playlists an export holds.

use crate::device::layout::Layout;
use crate::device::settings::Settings;
use crate::device::track::Track;
use crate::pdb::defaults;
use crate::pdb::ext::{ParentId, TagId, TagOrCategory, TagOrCategoryStrings, TrackTag};
use crate::pdb::io::Database;
use crate::pdb::offset_array::OffsetArrayContainer;
use crate::pdb::string::DeviceSQLString;
use crate::pdb::{
    Album, AlbumId, Artist, ArtistId, Artwork, ArtworkId, DatabaseType, Genre, GenreId, Key, KeyId,
    Label, LabelId, PlaylistEntry, PlaylistTreeNode, PlaylistTreeNodeId, Subtype, TrackId,
    TrackStrings, TrailingName,
};
use crate::setting::{Setting, SettingType};
use crate::util::MaybeCalculated;
use binrw::{BinRead, BinWrite};
use fallible_iterator::FallibleIterator;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::num::NonZero;
use std::path::{Path, PathBuf};

/// Represents a Rekordbox device export.
#[derive(Debug, PartialEq)]
pub struct DeviceExportLoader(PathBuf);

impl DeviceExportLoader {
    /// Load device export from the given path.
    ///
    /// The path should contain a `PIONEER` directory.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// Get the device path.
    #[must_use]
    pub fn get_path(&self) -> &Path {
        &self.0
    }

    fn read_setting_file(path: &PathBuf, setting_type: SettingType) -> crate::Result<Setting> {
        let mut reader = std::fs::File::open(path)?;
        let setting = Setting::read_args(&mut reader, (setting_type,))?;
        Ok(setting)
    }

    /// The paths of the export.
    #[must_use]
    pub fn layout(&self) -> Layout {
        Layout::new(self.0.clone())
    }

    /// Load setting files. If a file is missing or cannot be read,
    /// a warning will be printed.
    #[must_use]
    pub fn load_settings(&self) -> Settings {
        let layout = self.layout();

        let load_setting = |setting_type: SettingType| -> Option<Setting> {
            let file_path = layout.setting_file(setting_type);
            match Self::read_setting_file(&file_path, setting_type) {
                Ok(setting) => Some(setting),
                Err(e) => {
                    eprintln!("Warning: Could not load {}: {}", file_path.display(), e);
                    None
                }
            }
        };

        let mut settings = Settings::default();
        if let Some(devsetting) = load_setting(SettingType::DevSetting) {
            settings.set_devsetting(devsetting.data.as_dev_setting().unwrap());
        }
        if let Some(djmmysetting) = load_setting(SettingType::DJMMySetting) {
            settings.set_djmmysetting(djmmysetting.data.as_djm_my_setting().unwrap());
        }
        if let Some(mysetting) = load_setting(SettingType::MySetting) {
            settings.set_mysetting(mysetting.data.as_my_setting().unwrap());
        }
        if let Some(mysetting2) = load_setting(SettingType::MySetting2) {
            settings.set_mysetting2(mysetting2.data.as_my_setting2().unwrap());
        }

        settings
    }

    /// Open a PDB database without persistence back to disk.
    /// Still allows modifying data in memory.
    pub fn open_pdb_non_persistent(&self) -> crate::Result<Database<File>> {
        let path = self.layout().export_pdb();
        Database::open_non_persistent(File::open(path)?, DatabaseType::Plain)
    }
}

/// Unused page heap space left after a track row.
///
/// Rekordbox leaves this much space after the track rows of the exports we have fixtures for,
/// where a row needing about 290 bytes was given 336. Players have been reported to crash on
/// track rows that were given too little space, so this is not only a matter of writing the same
/// bytes as Rekordbox.
const TRACK_SLACK_BYTES: u16 = 48;

/// A tag that tracks can be labelled with, or a category holding tags.
///
/// An export holds the tag set of the library it was written from, keeping the ids that library
/// uses, so a tag is given with the id it already has rather than being given a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Id of the tag, which the tracks carrying it refer to it by.
    pub id: u32,
    /// Id of the category holding the tag, or zero for a category.
    pub parent_id: u32,
    /// Name of the tag.
    pub name: String,
    /// Place of the tag among the tags of its category, counted from zero.
    pub position: u32,
    /// Whether this is a category rather than a tag.
    pub is_category: bool,
}

/// The setting types that an export holds a file for.
///
/// Only the settings of the device are written. The other three files hold the settings of a
/// player, which Rekordbox copies to a stick when they are changed rather than when an export is
/// written, so an export it writes holds this one alone.
const SETTING_TYPES: [SettingType; 1] = [SettingType::DevSetting];

/// Writes a device export.
///
/// Creating one lays out the files that every export has: the two databases, holding the content
/// that Rekordbox puts in every export, and the four settings files, holding the default
/// settings. The databases stay open so that content can be added to them, and are written back
/// to the device by [`DeviceExportWriter::close`].
#[derive(Debug)]
pub struct DeviceExportWriter {
    layout: Layout,
    plain: Database<File>,
    ext: Database<File>,
    /// The id a track was given, by the path of its audio file, so that a track that is added
    /// twice does not end up in the export twice.
    track_ids: HashMap<String, TrackId>,
    /// The rows that tracks refer to by name, so that they are only added once each.
    artist_ids: HashMap<String, ArtistId>,
    album_ids: HashMap<String, AlbumId>,
    artwork_ids: HashMap<String, ArtworkId>,
    genre_ids: HashMap<String, GenreId>,
    label_ids: HashMap<String, LabelId>,
    key_ids: HashMap<String, KeyId>,
    /// The number of tag rows written, which the index of the next one follows from.
    num_tags: u32,
    /// The number of tags in a category, which the place of the next one follows from.
    tag_positions: HashMap<u32, u32>,
    /// The tags a track is labelled with, so that one is not held twice.
    track_tags: HashSet<(TrackId, TagId)>,
    /// The nodes of the playlist tree by the folder holding them and their name, so that a
    /// playlist that is already in the export is added to rather than added again. Two playlists
    /// in different folders may have the same name, which is why the folder is part of the key.
    playlist_ids: HashMap<(PlaylistTreeNodeId, String), PlaylistTreeNodeId>,
    /// The position of the last entry of a playlist, by its id.
    playlist_lengths: HashMap<PlaylistTreeNodeId, u32>,
    /// The tracks a playlist already holds, so that one is not put in it twice.
    playlist_members: HashMap<PlaylistTreeNodeId, HashSet<TrackId>>,
    /// The id to give the next row of each kind, which continues where an export that was opened
    /// left off.
    next_ids: NextIds,
}

/// The id to give the next row of each kind.
#[derive(Debug, Default)]
struct NextIds {
    track: u32,
    artist: u32,
    album: u32,
    artwork: u32,
    genre: u32,
    label: u32,
    key: u32,
    playlist: u32,
    tag: u32,
}

impl NextIds {
    /// The ids of an export that holds nothing, which are counted from one.
    fn initial() -> Self {
        Self {
            track: 1,
            artist: 1,
            album: 1,
            artwork: 1,
            genre: 1,
            label: 1,
            key: 1,
            playlist: 1,
            tag: 1,
        }
    }

    /// Takes the next id of a kind, and moves on to the one after it.
    fn take(counter: &mut u32) -> u32 {
        let id = *counter;
        *counter += 1;
        id
    }
}

impl DeviceExportWriter {
    /// Creates the files of an export in the given directory.
    ///
    /// `date` is the date the export is written, in the `YYYY-MM-DD` form that Rekordbox records
    /// it in. Fails without writing anything if the directory already holds an export, so that an
    /// export that took a long time to collect cannot be destroyed by accident.
    pub fn create(root: impl Into<PathBuf>, date: &str) -> crate::Result<Self> {
        Self::create_export(root, date, None)
    }

    /// Creates the files of an export, holding the given tag set.
    ///
    /// Rekordbox writes the tag set of the library an export comes from rather than the set it
    /// ships with, and keeps the ids of that library, so the set is given here with its ids
    /// rather than built up a tag at a time. An export created without one holds the set that
    /// Rekordbox ships with.
    pub fn create_with_tags(
        root: impl Into<PathBuf>,
        date: &str,
        tags: &[Tag],
    ) -> crate::Result<Self> {
        Self::create_export(root, date, Some(tags))
    }

    /// Creates the files of an export, with or without a tag set of its own.
    fn create_export(
        root: impl Into<PathBuf>,
        date: &str,
        tags: Option<&[Tag]>,
    ) -> crate::Result<Self> {
        let layout = Layout::new(root.into());
        std::fs::create_dir_all(layout.rekordbox_dir())?;

        for setting_type in SETTING_TYPES {
            let setting = match setting_type {
                SettingType::DevSetting => Setting::default_devsetting(),
                SettingType::DJMMySetting => Setting::default_djmmysetting(),
                SettingType::MySetting => Setting::default_mysetting(),
                SettingType::MySetting2 => Setting::default_mysetting2(),
            };
            write_setting_file(&layout.setting_file(setting_type), &setting)?;
        }

        let mut plain = create_database(&layout.export_pdb(), DatabaseType::Plain)?;
        defaults::insert_defaults(&mut plain, date, 0)?;
        let mut ext = create_database(&layout.export_ext_pdb(), DatabaseType::Ext)?;
        match tags {
            // The row of the table that is not understood comes first in the exports we have,
            // whichever tag set follows it.
            Some(tags) => {
                defaults::insert_unknown7(&mut ext)?;
                for tag in tags {
                    defaults::insert_tag(
                        &mut ext,
                        tag.id,
                        tag.parent_id,
                        tag.position,
                        tag.is_category,
                        &tag.name,
                    )?;
                }
            }
            None => defaults::insert_ext_defaults(&mut ext)?,
        }
        // The tag set that was just written has taken ids and rows of its own, which the tags of
        // a caller have to come after.
        let (num_tags, next_tag) = read_tags(&mut ext)?;

        Ok(Self {
            layout,
            plain,
            ext,
            track_ids: HashMap::new(),
            artist_ids: HashMap::new(),
            album_ids: HashMap::new(),
            artwork_ids: HashMap::new(),
            genre_ids: HashMap::new(),
            label_ids: HashMap::new(),
            key_ids: HashMap::new(),
            num_tags,
            tag_positions: HashMap::new(),
            track_tags: HashSet::new(),
            playlist_ids: HashMap::new(),
            playlist_lengths: HashMap::new(),
            playlist_members: HashMap::new(),
            next_ids: NextIds {
                tag: next_tag,
                ..NextIds::initial()
            },
        })
    }

    /// Opens an export that is already on a device, to add to it.
    ///
    /// What the export already holds is read back, so that a track whose audio file is already
    /// there keeps the id it has, the rows that tracks share are reused, and a playlist that is
    /// already there is added to rather than added again. That makes it possible to put a
    /// playlist that has grown on a device without copying what is on it already.
    pub fn open(root: impl Into<PathBuf>) -> crate::Result<Self> {
        let layout = Layout::new(root.into());
        let plain = open_database(&layout.export_pdb(), DatabaseType::Plain)?;
        let ext = open_database(&layout.export_ext_pdb(), DatabaseType::Ext)?;

        let mut writer = Self {
            layout,
            plain,
            ext,
            track_ids: HashMap::new(),
            artist_ids: HashMap::new(),
            album_ids: HashMap::new(),
            artwork_ids: HashMap::new(),
            genre_ids: HashMap::new(),
            label_ids: HashMap::new(),
            key_ids: HashMap::new(),
            num_tags: 0,
            tag_positions: HashMap::new(),
            track_tags: HashSet::new(),
            playlist_ids: HashMap::new(),
            playlist_lengths: HashMap::new(),
            playlist_members: HashMap::new(),
            next_ids: NextIds::initial(),
        };
        writer.read_existing()?;
        let (num_tags, next_tag) = read_tags(&mut writer.ext)?;
        writer.num_tags = num_tags;
        writer.next_ids.tag = next_tag;
        writer.track_tags = writer
            .ext
            .iter_rows::<TrackTag>()?
            .map(|row| Ok((row.track_id, row.tag_id)))
            .collect::<Vec<_>>()?
            .into_iter()
            .collect();
        Ok(writer)
    }

    /// Reads back what an export already holds.
    fn read_existing(&mut self) -> crate::Result<()> {
        // The rows are collected first, as reading them borrows the database.
        let tracks: Vec<(String, TrackId)> = self
            .plain
            .iter_rows::<crate::pdb::Track>()?
            .map(|row| Ok((row.offsets.file_path.clone().into_string()?, row.id)))
            .collect()?;
        let artists: Vec<(String, ArtistId)> = self
            .plain
            .iter_rows::<Artist>()?
            .map(|row| Ok((row.offsets.name.clone().into_string()?, row.id)))
            .collect()?;
        let albums: Vec<(String, AlbumId)> = self
            .plain
            .iter_rows::<Album>()?
            .map(|row| Ok((row.offsets.name.clone().into_string()?, row.id)))
            .collect()?;
        let genres: Vec<(String, GenreId)> = self
            .plain
            .iter_rows::<Genre>()?
            .map(|row| Ok((row.name.clone().into_string()?, row.id)))
            .collect()?;
        let labels: Vec<(String, LabelId)> = self
            .plain
            .iter_rows::<Label>()?
            .map(|row| Ok((row.name.clone().into_string()?, row.id)))
            .collect()?;
        let keys: Vec<(String, KeyId)> = self
            .plain
            .iter_rows::<Key>()?
            .map(|row| Ok((row.name.clone().into_string()?, row.id)))
            .collect()?;
        let artwork: Vec<(String, ArtworkId)> = self
            .plain
            .iter_rows::<Artwork>()?
            .map(|row| Ok((row.path.clone().into_string()?, row.id)))
            .collect()?;
        let playlists: Vec<(PlaylistTreeNodeId, String, PlaylistTreeNodeId)> = self
            .plain
            .iter_rows::<PlaylistTreeNode>()?
            .map(|row| Ok((row.parent_id, row.name.clone().into_string()?, row.id)))
            .collect()?;
        let entries: Vec<(PlaylistTreeNodeId, u32, TrackId)> = self
            .plain
            .iter_rows::<PlaylistEntry>()?
            .map(|row| Ok((row.playlist_id, row.entry_index, row.track_id)))
            .collect()?;

        // An id that is already taken must not be given out again.
        self.next_ids.track = next_after(tracks.iter().map(|(_, id)| id.0));
        self.next_ids.artist = next_after(artists.iter().map(|(_, id)| id.0));
        self.next_ids.album = next_after(albums.iter().map(|(_, id)| id.0));
        self.next_ids.genre = next_after(genres.iter().map(|(_, id)| id.0));
        self.next_ids.label = next_after(labels.iter().map(|(_, id)| id.0));
        self.next_ids.key = next_after(keys.iter().map(|(_, id)| id.0));
        self.next_ids.artwork = next_after(artwork.iter().map(|(_, id)| id.0));
        self.next_ids.playlist = next_after(playlists.iter().map(|(_, _, id)| id.0));

        self.track_ids = tracks.into_iter().collect();
        self.artist_ids = artists.into_iter().collect();
        self.album_ids = albums.into_iter().collect();
        self.genre_ids = genres.into_iter().collect();
        self.label_ids = labels.into_iter().collect();
        self.key_ids = keys.into_iter().collect();
        self.artwork_ids = artwork.into_iter().collect();
        self.playlist_ids = playlists
            .iter()
            .map(|(parent, name, id)| ((*parent, name.clone()), *id))
            .collect();
        for (_, _, id) in playlists {
            self.playlist_lengths.entry(id).or_insert(0);
            self.playlist_members.entry(id).or_default();
        }
        for (playlist_id, position, track_id) in entries {
            let length = self.playlist_lengths.entry(playlist_id).or_insert(0);
            *length = (*length).max(position);
            self.playlist_members
                .entry(playlist_id)
                .or_default()
                .insert(track_id);
        }
        Ok(())
    }

    /// The paths of the export being written.
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The database holding the tracks, playlists and the metadata they are browsed by.
    pub fn plain_database(&mut self) -> &mut Database<File> {
        &mut self.plain
    }

    /// The database holding the extended track information.
    pub fn ext_database(&mut self) -> &mut Database<File> {
        &mut self.ext
    }

    /// Adds a track to the export.
    ///
    /// The artist, album, genre, label and key of the track are added to the export as well if
    /// they are not in it yet, and shared with the tracks added before. A track whose audio file
    /// was added before is not added again, and keeps the id it was given then.
    ///
    /// Returns the id the track has in the export, which playlists refer to it by.
    pub fn add_track(&mut self, track: &Track) -> crate::Result<TrackId> {
        if let Some(&id) = self.track_ids.get(&track.file_path) {
            return Ok(id);
        }

        let artist_id = self.add_artist(&track.artist)?;
        // An album names its own artist, which is the artist of the track where the track names
        // none of its own.
        let album_artist_id = if track.album_artist.is_empty() {
            artist_id
        } else {
            self.add_artist(&track.album_artist)?
        };
        let album_id = self.add_album(&track.album, album_artist_id)?;
        let genre_id = self.add_genre(&track.genre)?;
        let label_id = self.add_label(&track.label)?;
        let key_id = self.add_key(&track.key)?;
        let artwork_id = self.add_artwork(&track.artwork_path)?;

        let id = TrackId(NextIds::take(&mut self.next_ids.track));
        let row = crate::pdb::Track {
            id,
            artist_id,
            album_id,
            genre_id,
            label_id,
            key_id,
            artwork_id,
            sample_rate: track.sample_rate,
            sample_depth: track.sample_depth,
            file_size: track.file_size,
            bitrate: track.bitrate,
            track_number: track.track_number,
            disc_number: track.disc_number,
            year: track.year,
            duration: track.duration_secs,
            rating: track.rating.to_byte(),
            play_count: track.play_count,
            color: track.color.clone(),
            file_type: track.file_type.clone(),
            // The tempo is stored in hundredths of a beat per minute.
            tempo: tempo_hundredths(track.tempo).ok_or(crate::Error::IntegrityError(
                "the tempo of a track is not a number of beats per minute that can be recorded",
            ))?,
            offsets: OffsetArrayContainer {
                offsets: MaybeCalculated::Calculated,
                inner: TrackStrings {
                    // Rekordbox keeps a counter beside the play count, which holds the count
                    // plus one in the exports we have to go by.
                    unknown_string2: DeviceSQLString::new(
                        &track.play_count.saturating_add(1).to_string(),
                    )?,
                    title: DeviceSQLString::new(&track.title)?,
                    file_path: DeviceSQLString::new(&track.file_path)?,
                    filename: DeviceSQLString::new(file_name_of(&track.file_path))?,
                    comment: DeviceSQLString::new(&track.comment)?,
                    date_added: DeviceSQLString::new(&track.date_added)?,
                    release_date: DeviceSQLString::new(&track.release_date)?,
                    mix_name: DeviceSQLString::new(&track.mix_name)?,
                    analyze_path: DeviceSQLString::new(&track.analyze_path)?,
                    analyze_date: DeviceSQLString::new(&track.analyze_date)?,
                    ..Default::default()
                },
            },
            ..Default::default()
        };
        self.plain.add_row_with_slack(row, TRACK_SLACK_BYTES)?;

        self.track_ids.insert(track.file_path.clone(), id);
        Ok(id)
    }

    /// Adds a category that tags can be put in, which players show as a set of its own.
    ///
    /// Returns the id of the category, which [`DeviceExportWriter::create_tag`] takes.
    pub fn create_tag_category(&mut self, name: &str) -> crate::Result<TagId> {
        self.add_tag_row(None, name)
    }

    /// Adds a tag to a category, which tracks can be labelled with.
    pub fn create_tag(&mut self, category: TagId, name: &str) -> crate::Result<TagId> {
        self.add_tag_row(Some(category), name)
    }

    /// Labels a track with a tag, and says whether the track carried it already.
    pub fn add_tag_to_track(&mut self, track_id: TrackId, tag_id: TagId) -> crate::Result<bool> {
        if !self.track_tags.insert((track_id, tag_id)) {
            return Ok(false);
        }
        self.ext.add_row_with_slack(
            TrackTag {
                track_id,
                tag_id,
                // Holds a three in every export we have seen.
                unknown_const: TrackTag::UNKNOWN_CONST,
            },
            0,
        )?;
        Ok(true)
    }

    /// Adds a row to the tag table, as a category when it is given no category to sit in.
    fn add_tag_row(&mut self, category: Option<TagId>, name: &str) -> crate::Result<TagId> {
        let id = TagId(NextIds::take(&mut self.next_ids.tag));
        // A tag holds its place among the tags of its category, counted from zero.
        let within = self
            .tag_positions
            .entry(category.map_or(0, |category| category.0))
            .or_insert(0);
        let position = *within;
        *within += 1;

        self.ext.add_row_with_slack(
            TagOrCategory {
                subtype: Subtype(TagOrCategory::SUBTYPE),
                // Recorded when the row is stored, from the place it takes in its page.
                index_shift: 0,
                unknown1: 0,
                unknown2: 0,
                parent_id: ParentId(category.and_then(|category| NonZero::new(category.0))),
                position,
                id,
                raw_is_category: if category.is_none() {
                    TagOrCategory::IS_CATEGORY
                } else {
                    0
                },
                offsets: OffsetArrayContainer {
                    offsets: MaybeCalculated::Calculated,
                    inner: TagOrCategoryStrings {
                        name: DeviceSQLString::new(name)?,
                        unknown: DeviceSQLString::empty(),
                    },
                },
            },
            TagOrCategory::SLACK_BYTES,
        )?;
        self.num_tags += 1;
        Ok(id)
    }

    /// Adds a folder to the playlist tree, which playlists and further folders can be put in.
    ///
    /// Returns the id of the folder, which is what `parent` takes to put something in it.
    pub fn create_playlist_folder(
        &mut self,
        parent: Option<PlaylistTreeNodeId>,
        name: &str,
    ) -> crate::Result<PlaylistTreeNodeId> {
        self.create_playlist_node(parent, name, true)
    }

    /// Adds a playlist to the playlist tree, which tracks can be added to.
    ///
    /// Returns the id of the playlist, which is what [`DeviceExportWriter::add_track_to_playlist`]
    /// takes.
    pub fn create_playlist(
        &mut self,
        parent: Option<PlaylistTreeNodeId>,
        name: &str,
    ) -> crate::Result<PlaylistTreeNodeId> {
        self.create_playlist_node(parent, name, false)
    }

    /// Adds a track to the end of a playlist.
    pub fn add_track_to_playlist(
        &mut self,
        playlist_id: PlaylistTreeNodeId,
        track_id: TrackId,
    ) -> crate::Result<bool> {
        let length = self.playlist_lengths.get(&playlist_id).copied().ok_or(
            crate::Error::IntegrityError("no playlist of the export has the given id"),
        )?;
        if self
            .playlist_members
            .get(&playlist_id)
            .is_some_and(|members| members.contains(&track_id))
        {
            return Ok(false);
        }

        self.plain.add_row(PlaylistEntry {
            // Positions in a playlist are counted from one.
            entry_index: length + 1,
            track_id,
            playlist_id,
        })?;
        self.playlist_lengths.insert(playlist_id, length + 1);
        self.playlist_members
            .entry(playlist_id)
            .or_default()
            .insert(track_id);
        Ok(true)
    }

    /// The node of the playlist tree with the given name, in the given folder or at the root of
    /// the tree, if the export holds one.
    #[must_use]
    pub fn playlist_by_name(
        &self,
        parent: Option<PlaylistTreeNodeId>,
        name: &str,
    ) -> Option<PlaylistTreeNodeId> {
        self.playlist_ids
            .get(&(parent.unwrap_or(PlaylistTreeNodeId(0)), name.to_owned()))
            .copied()
    }

    /// Adds a node to the playlist tree, which is either a folder or a playlist.
    fn create_playlist_node(
        &mut self,
        parent: Option<PlaylistTreeNodeId>,
        name: &str,
        is_folder: bool,
    ) -> crate::Result<PlaylistTreeNodeId> {
        if let Some(parent) = parent {
            if !self.playlist_lengths.contains_key(&parent) {
                return Err(crate::Error::IntegrityError(
                    "no node of the playlist tree has the given id",
                ));
            }
        }

        let index = self.next_ids.playlist - 1;
        let id = PlaylistTreeNodeId(NextIds::take(&mut self.next_ids.playlist));
        self.plain.add_row(PlaylistTreeNode {
            // A node with no parent sits at the root of the tree, which is what a parent of zero
            // means.
            parent_id: parent.unwrap_or(PlaylistTreeNodeId(0)),
            sort_order: index,
            id,
            node_is_folder: u32::from(is_folder),
            name: DeviceSQLString::new(name)?,
            ..Default::default()
        })?;

        self.playlist_ids.insert(
            (parent.unwrap_or(PlaylistTreeNodeId(0)), name.to_owned()),
            id,
        );
        self.playlist_lengths.insert(id, 0);
        self.playlist_members.entry(id).or_default();
        Ok(id)
    }

    /// Adds the image holding the artwork of a track unless it is already in the export, and
    /// returns its id.
    fn add_artwork(&mut self, source_path: &str) -> crate::Result<ArtworkId> {
        if source_path.is_empty() {
            return Ok(ArtworkId(0));
        }
        if let Some(&id) = self.artwork_ids.get(source_path) {
            return Ok(id);
        }

        let id = ArtworkId(NextIds::take(&mut self.next_ids.artwork));
        // Where the image belongs on the device follows from the id, so the row names that
        // rather than wherever the image came from.
        let on_device = self
            .layout
            .artwork_file(id.0)
            .strip_prefix(self.layout.root())
            .map(|path| format!("/{}", path.display()))
            .unwrap_or_default();
        self.plain.add_row(Artwork {
            id,
            path: DeviceSQLString::new(&on_device)?,
        })?;
        self.artwork_ids.insert(source_path.to_owned(), id);
        Ok(id)
    }

    /// Where the image that a track names belongs on the device, once the track has been added.
    #[must_use]
    pub fn artwork_file(&self, source_path: &str) -> Option<PathBuf> {
        self.artwork_ids
            .get(source_path)
            .map(|&id| self.layout.artwork_file(id.0))
    }

    /// Adds an artist to the export unless it is already in it, and returns its id.
    fn add_artist(&mut self, name: &str) -> crate::Result<ArtistId> {
        if name.is_empty() {
            return Ok(ArtistId(0));
        }
        if let Some(&id) = self.artist_ids.get(name) {
            return Ok(id);
        }

        let id = ArtistId(NextIds::take(&mut self.next_ids.artist));
        self.plain.add_row(Artist {
            id,
            offsets: OffsetArrayContainer {
                offsets: MaybeCalculated::Calculated,
                inner: TrailingName {
                    name: DeviceSQLString::new(name)?,
                },
            },
            ..Default::default()
        })?;
        self.artist_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Adds an album to the export unless it is already in it, and returns its id.
    fn add_album(&mut self, name: &str, artist_id: ArtistId) -> crate::Result<AlbumId> {
        if name.is_empty() {
            return Ok(AlbumId(0));
        }
        if let Some(&id) = self.album_ids.get(name) {
            return Ok(id);
        }

        let id = AlbumId(NextIds::take(&mut self.next_ids.album));
        self.plain.add_row(Album {
            id,
            artist_id,
            offsets: OffsetArrayContainer {
                offsets: MaybeCalculated::Calculated,
                inner: TrailingName {
                    name: DeviceSQLString::new(name)?,
                },
            },
            ..Default::default()
        })?;
        self.album_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Adds a genre to the export unless it is already in it, and returns its id.
    fn add_genre(&mut self, name: &str) -> crate::Result<GenreId> {
        if name.is_empty() {
            return Ok(GenreId(0));
        }
        if let Some(&id) = self.genre_ids.get(name) {
            return Ok(id);
        }

        let id = GenreId(NextIds::take(&mut self.next_ids.genre));
        self.plain.add_row(Genre {
            id,
            name: DeviceSQLString::new(name)?,
        })?;
        self.genre_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Adds a record label to the export unless it is already in it, and returns its id.
    fn add_label(&mut self, name: &str) -> crate::Result<LabelId> {
        if name.is_empty() {
            return Ok(LabelId(0));
        }
        if let Some(&id) = self.label_ids.get(name) {
            return Ok(id);
        }

        let id = LabelId(NextIds::take(&mut self.next_ids.label));
        self.plain.add_row(Label {
            id,
            name: DeviceSQLString::new(name)?,
        })?;
        self.label_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Adds a musical key to the export unless it is already in it, and returns its id.
    fn add_key(&mut self, name: &str) -> crate::Result<KeyId> {
        if name.is_empty() {
            return Ok(KeyId(0));
        }
        if let Some(&id) = self.key_ids.get(name) {
            return Ok(id);
        }

        let index = self.next_ids.key - 1;
        let id = KeyId(NextIds::take(&mut self.next_ids.key));
        self.plain.add_row(Key {
            id,
            // The second copy of the id that a key row holds.
            id2: index + 1,
            name: DeviceSQLString::new(name)?,
        })?;
        self.key_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Writes the databases to the device and makes sure everything written has reached it.
    ///
    /// Call this before the device is unmounted, as nothing guarantees that the export has been
    /// written otherwise.
    pub fn close(mut self) -> crate::Result<()> {
        self.plain.flush()?;
        self.ext.flush()?;
        // The databases point at the analysis and audio files, so they are the last thing that
        // should reach the device.
        self.ext.into_inner().sync_all()?;
        self.plain.into_inner().sync_all()?;
        Ok(())
    }
}

/// A tempo in the hundredths of a beat per minute that a track row counts it in, or `None` for
/// one that cannot be counted that way, such as a tempo that is not a number at all.
fn tempo_hundredths(tempo: f32) -> Option<u32> {
    let hundredths = (f64::from(tempo) * 100.0).round();
    if !hundredths.is_finite() || !(0.0..=f64::from(u32::MAX)).contains(&hundredths) {
        return None;
    }
    // Whole, finite and within range, as checked just above.
    Some(hundredths as u32)
}

/// The name of the file a path names, which a track row holds alongside the path itself.
fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The id to give out next, after the ids that are already taken.
fn next_after(ids: impl Iterator<Item = u32>) -> u32 {
    ids.max().unwrap_or(0) + 1
}

/// The number of tag rows a database holds, and the id to give the next one.
fn read_tags(db: &mut Database<File>) -> crate::Result<(u32, u32)> {
    let ids: Vec<u32> = db
        .iter_rows::<TagOrCategory>()?
        .map(|row| Ok(row.id.0))
        .collect()?;
    let count = u32::try_from(ids.len()).map_err(|_| {
        crate::Error::IntegrityError("the tag table holds more rows than can be counted")
    })?;
    Ok((count, next_after(ids.into_iter())))
}

/// Opens a database that a device already holds, for reading and writing.
fn open_database(path: &Path, db_type: DatabaseType) -> crate::Result<Database<File>> {
    let file = File::options().read(true).write(true).open(path)?;
    Database::open(file, db_type)
}

/// Creates a database file and an empty database in it.
fn create_database(path: &Path, db_type: DatabaseType) -> crate::Result<Database<File>> {
    let file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    Database::create(file, db_type)
}

/// Writes a settings file and makes sure it has reached the device.
fn write_setting_file(path: &Path, setting: &Setting) -> crate::Result<()> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    setting.write(&mut file)?;
    file.sync_all()?;
    Ok(())
}
