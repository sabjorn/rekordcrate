// Copyright (c) 2026 Jan Holthuis <jan.holthuis@rub.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy
// of the MPL was not distributed with this file, You can obtain one at
// http://mozilla.org/MPL/2.0/.
//
// SPDX-License-Identifier: MPL-2.0

//! On-disk layout of a Rekordbox device export: where `export.pdb`, `exportExt.pdb`, the
//! `SETTING.DAT` files and the `PIONEER`/`USBANLZ`/`Contents` directories live relative
//! to the device root.

use crate::anlz::AnalysisPath;
use crate::setting::SettingType;
use std::path::{Path, PathBuf};

/// The `*SETTING.DAT` files in a device export, in the order Rekordbox writes them.
pub const DAT_FILES: &[(&str, SettingType)] = &[
    ("DEVSETTING.DAT", SettingType::DevSetting),
    ("DJMMYSETTING.DAT", SettingType::DJMMySetting),
    ("MYSETTING.DAT", SettingType::MySetting),
    ("MYSETTING2.DAT", SettingType::MySetting2),
];

/// On-disk layout of a device export rooted at `root`. Derives all paths from it on demand.
///
/// Exposed so expert callers can locate [`Self::export_pdb`] / [`Self::export_ext_pdb`] and the
/// surrounding `PIONEER` / `Contents` directories directly, for manual inspection or modification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Wrap a device-export root directory.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The device root directory this layout was built from.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `PIONEER` directory.
    #[must_use]
    pub fn pioneer_dir(&self) -> PathBuf {
        self.root.join("PIONEER")
    }

    /// The `PIONEER/rekordbox` directory holding the PDB files.
    #[must_use]
    pub fn rekordbox_dir(&self) -> PathBuf {
        self.pioneer_dir().join("rekordbox")
    }

    /// Path to `export.pdb`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rekordcrate::device::layout::Layout;
    /// let layout = Layout::new("/srv/export".into());
    /// assert!(layout.export_pdb().ends_with("export.pdb"));
    /// ```
    #[must_use]
    pub fn export_pdb(&self) -> PathBuf {
        self.rekordbox_dir().join("export.pdb")
    }

    /// Path to `exportExt.pdb`.
    #[must_use]
    pub fn export_ext_pdb(&self) -> PathBuf {
        self.rekordbox_dir().join("exportExt.pdb")
    }

    /// The `PIONEER/USBANLZ` directory holding per-track analysis files.
    #[must_use]
    pub fn usbanlz_dir(&self) -> PathBuf {
        self.pioneer_dir().join("USBANLZ")
    }

    /// The `Contents` directory holding audio files.
    #[must_use]
    pub fn contents_dir(&self) -> PathBuf {
        self.root.join("Contents")
    }

    /// Path to a `*SETTING.DAT` file by name, under `PIONEER`.
    #[must_use]
    pub fn dat_path(&self, filename: &str) -> PathBuf {
        self.pioneer_dir().join(filename)
    }

    /// The `PIONEER/Artwork` directory holding track artwork.
    #[must_use]
    pub fn artwork_dir(&self) -> PathBuf {
        self.pioneer_dir().join("Artwork")
    }

    /// Path to the 80×80 thumbnail `a{id}.jpg` of artwork `id`, the file an `Artwork` row names.
    #[must_use]
    pub fn artwork_file(&self, id: u32) -> PathBuf {
        self.artwork_dir()
            .join(artwork_folder(id))
            .join(format!("a{id}.jpg"))
    }

    /// Path to the 240×240 `a{id}_m.jpg` of artwork `id`, which players look for next to the
    /// thumbnail.
    #[must_use]
    pub fn artwork_m_file(&self, id: u32) -> PathBuf {
        self.artwork_dir()
            .join(artwork_folder(id))
            .join(format!("a{id}_m.jpg"))
    }

    /// Where the audio file of a track belongs on the device.
    ///
    /// Rekordbox holds the audio of a track under the artist and the album it names, cutting
    /// each name to what a path allows, and a track naming no album is held under a name
    /// standing for that.
    #[must_use]
    pub fn contents_file(&self, artist: &str, album: &str, filename: &str) -> PathBuf {
        let directory = |name: &str, unknown: &str| {
            let name = name.trim();
            let name = if name.is_empty() { unknown } else { name };
            cut_to_length(&replace_path_characters(name), MAX_PATH_PART)
        };
        self.contents_dir()
            .join(directory(artist, UNKNOWN_ARTIST))
            .join(directory(album, UNKNOWN_ALBUM))
            .join(cut_file_name(&replace_path_characters(filename)))
    }

    /// The directory holding the analysis files of the track with the given audio file.
    ///
    /// The path is the path of the audio file on the device, as the track row holds it, e.g.
    /// `/Contents/Artist/Album/Track.mp3`.
    #[must_use]
    pub fn anlz_dir_for_audio(&self, device_audio_path: &str) -> PathBuf {
        self.usbanlz_dir()
            .join(AnalysisPath::for_audio_path(device_audio_path).relative_dir())
    }

    /// The analysis file with the given extension of the track with the given audio file.
    ///
    /// The extension is one of `DAT`, `EXT` or `2EX`.
    #[must_use]
    pub fn anlz_file_for_audio(&self, device_audio_path: &str, extension: &str) -> PathBuf {
        self.anlz_dir_for_audio(device_audio_path)
            .join(format!("{}.{extension}", AnalysisPath::FILE_STEM))
    }

    /// The profile of the DJ, which an export carries a copy of.
    #[must_use]
    pub fn dj_profile(&self) -> PathBuf {
        self.pioneer_dir().join("djprofile.nxs")
    }

    /// The file holding settings of the given type.
    #[must_use]
    pub fn setting_file(&self, setting_type: SettingType) -> PathBuf {
        self.pioneer_dir().join(setting_type.filename())
    }
}

/// Five-digit folder name Rekordbox files artwork `id` under, zero-padded.
#[must_use]
pub fn artwork_folder(id: u32) -> String {
    format!("{:05}", id / ARTWORK_PER_DIRECTORY + 1)
}

/// Longest a part of a path below `Contents` is allowed to be.
///
/// Rekordbox cuts the name of an artist, an album or a file to this, which the exports it writes
/// bear out: nothing in them is longer, and the names it had to cut end exactly here.
const MAX_PATH_PART: usize = 48;

/// Number of images that Rekordbox holds in one directory of artwork.
const ARTWORK_PER_DIRECTORY: u32 = 20;

/// Name standing in for the album of a track that names none, as Rekordbox writes it.
const UNKNOWN_ALBUM: &str = "UnknownAlbum";

/// Name standing in for the artist of a track that names none.
///
/// No export we have holds a track without an artist, so this follows the name used for an
/// album.
const UNKNOWN_ARTIST: &str = "UnknownArtist";

/// Replaces the characters that a part of a path cannot hold.
fn replace_path_characters(name: &str) -> String {
    name.replace(['/', ':'], "_")
}

/// Cuts a name to a number of characters, counting characters rather than bytes so that a name
/// holding more than ASCII is not cut through the middle of one.
fn cut_to_length(name: &str, length: usize) -> String {
    name.chars().take(length).collect()
}

/// Cuts the name of a file, keeping the extension so that the file is still what it was.
fn cut_file_name(filename: &str) -> String {
    if filename.chars().count() <= MAX_PATH_PART {
        return filename.to_owned();
    }
    match filename.rsplit_once('.') {
        Some((stem, extension)) => {
            let room = MAX_PATH_PART.saturating_sub(extension.chars().count() + 1);
            format!("{}.{extension}", cut_to_length(stem, room))
        }
        None => cut_to_length(filename, MAX_PATH_PART),
    }
}
