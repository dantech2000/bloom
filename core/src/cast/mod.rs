// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote control between the sessions of a Jellyfin server ("Play on"),
//! without the app: the shapes on the wire and what a command asks of a
//! player (`protocol`). The app adds the target, the panel and the debug
//! channel (`src/cast`).

pub mod protocol;
