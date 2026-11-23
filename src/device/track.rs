// Copyright (c) 2026 Jan Holthuis <jan.holthuis@rub.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy
// of the MPL was not distributed with this file, You can obtain one at
// http://mozilla.org/MPL/2.0/.
//
// SPDX-License-Identifier: MPL-2.0

//! The [`Track`] type: a track as a user thinks of it, used both to add tracks to a
//! [`DeviceExportWriter`](crate::DeviceExportWriter) and to read them back.

use crate::util::{ColorIndex, FileType, Rating};

/// A track as a user thinks of it: plain `String` fields, no foreign-key IDs. An empty string
/// means "none", as in the PDB itself.
///
/// [`DeviceExportWriter::add_track`](crate::DeviceExportWriter::add_track) takes one and
/// [`DeviceExportWriter`](crate::DeviceExportWriter) hands them back. Fields not exposed
/// here need a `pdb::Track` row driven directly via [`crate::pdb::io::Database`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    /// Track title.
    pub title: String,
    /// Performing artist name.
    pub artist: String,
    /// Album name.
    pub album: String,
    /// Genre name.
    pub genre: String,
    /// Musical key name (e.g. "Am", "F#m", "8A"), stored as given.
    pub key: String,
    /// Record label name.
    pub label: String,
    /// Composer name.
    pub composer: String,
    /// Remixer name.
    pub remixer: String,
    /// Original performer, distinct from `artist` (covers/reworks).
    pub orig_artist: String,
    /// Free-text comment. Padded with trailing spaces when the row would fall under the minimum
    /// size some players need; trailing spaces are trimmed when reading back.
    pub comment: String,
    /// ISRC, in rekordbox's mangled format.
    pub isrc: String,
    /// Lyricist name.
    pub lyricist: String,
    /// Remix/mix name.
    pub mix_name: String,
    /// Free text. Rekordbox itself writes strict `YYYY-MM-DD` or empty here, but this field is
    /// passed through verbatim so callers can probe hardware behavior with arbitrary strings.
    pub release_date: String,
    /// Free text, commonly `YYYY-MM-DD` (see [`Self::release_date`]).
    pub date_added: String,
    /// Device-relative file path (e.g. `/Contents/Artist - Title.mp3`). Dedup key when non-empty.
    pub file_path: String,
    /// File name without path.
    pub filename: String,
    /// Device path of the artwork thumbnail (e.g. `/PIONEER/Artwork/00001/a1.jpg`), stored
    /// verbatim in the `Artwork` row. Empty = none. The caller owns placing the image files it
    /// names — or, with the `artwork` feature, gets a path whose files the export writes itself
    /// from `DeviceExport::import_artwork`.
    pub artwork_path: String,
    /// Track "message" field shown in Rekordbox.
    pub message: String,
    /// Tempo in BPM; encoded to centi-BPM (× 100) in the PDB.
    pub tempo: f32,
    /// Bitrate in kbps.
    pub bitrate: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Bits per sample of the audio file.
    pub sample_depth: u16,
    /// Playback duration in seconds. PDB field is `u16` (ceiling ~18.2 h).
    pub duration_secs: u16,
    /// File size in bytes.
    pub file_size: u32,
    /// Track number within the album.
    pub track_number: u32,
    /// Disc number.
    pub disc_number: u16,
    /// Release year.
    pub year: u16,
    /// Number of times the track was played.
    pub play_count: u16,
    /// Star rating.
    pub rating: Rating,
    /// Color label.
    pub color: ColorIndex,
    /// Audio file format.
    pub file_type: FileType,
    /// Whether stored hotcues auto-load on a CDJ. Maps to the PDB string `"ON"` / empty.
    pub autoload_hotcues: bool,
    /// Name of the artist of the album, which an album names rather than naming the artist of a
    /// track on it. Falls back to the artist of the track when empty.
    pub album_artist: String,
    /// Path of the analysis file of the track on the device, below `/PIONEER/USBANLZ`.
    pub analyze_path: String,
    /// Date the track was analyzed, in `YYYY-MM-DD` form.
    pub analyze_date: String,
}
