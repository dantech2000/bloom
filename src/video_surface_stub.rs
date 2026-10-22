// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The video frame type for a platform with no video path yet (iOS):
//! the pages keep their `Option<VideoFrame>` and never get one.

/// One video frame on the GPU. Never made on this platform.
#[derive(Clone)]
pub struct VideoFrame(());

impl VideoFrame {
    /// The buffer a page would hand to the surface element; the element
    /// is the Mac's, and the pages skip it here.
    pub fn buffer(&self) -> VideoFrame {
        self.clone()
    }
}
