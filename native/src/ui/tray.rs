// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK-side tray/background decisions. The StatusNotifierItem service itself
//! is WR-002's `tray_backend`; this module only decides when the window may
//! hide, and never hides the only window when no watcher is available.

/// Should the window stay hidden at startup? Only with a live watcher —
/// otherwise the process would be unreachable.
pub fn start_hidden(watcher_available: bool, start_minimized: bool) -> bool {
    watcher_available && start_minimized
}

/// Should closing the window hide instead of quit?
pub fn close_hides(watcher_available: bool, close_to_tray: bool) -> bool {
    watcher_available && close_to_tray
}

/// Should minimizing the window hide it to the tray?
pub fn minimize_hides(watcher_available: bool, minimize_to_tray: bool) -> bool {
    watcher_available && minimize_to_tray
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_watcher_never_hides_the_only_window() {
        assert!(!start_hidden(false, true));
        assert!(!close_hides(false, true));
        assert!(!minimize_hides(false, true));
    }

    #[test]
    fn watcher_plus_settings_allow_hiding() {
        assert!(start_hidden(true, true));
        assert!(!start_hidden(true, false));
        assert!(close_hides(true, true));
        assert!(!close_hides(true, false));
        assert!(minimize_hides(true, true));
        assert!(!minimize_hides(true, false));
    }
}
