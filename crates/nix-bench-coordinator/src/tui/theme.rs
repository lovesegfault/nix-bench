//! Centralized theme and color palette for TUI
//!
//! Provides a btop-inspired rich color scheme using Catppuccin Mocha palette.

use crate::orchestrator::InstanceStatus;
use ratatui::style::{Color, Modifier, Style};

/// Theme configuration with all colors for the TUI
pub struct Theme {
    // Base colors
    pub bg: Color,
    pub fg: Color,
    pub fg_dim: Color,

    // Accent colors (rich gradients like btop)
    pub accent_primary: Color,   // Blue - main accent
    pub accent_secondary: Color, // Lavender - secondary accent
    pub accent_tertiary: Color,  // Yellow/peach - highlights

    // Semantic colors
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,

    // Status-specific (for instance states)
    pub status_pending: Color,
    pub status_launching: Color,
    pub status_running: Color,
    pub status_complete: Color,
    pub status_failed: Color,

    // Log level colors (for colorized tracing)
    pub log_trace: Color,
    pub log_debug: Color,
    pub log_info: Color,
    pub log_warn: Color,
    pub log_error: Color,

    // UI elements
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub selection_bg: Color,
    pub highlight: Color,
    pub header_bg: Color,
    pub scrollbar_track: Color,
    pub scrollbar_thumb: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

impl Theme {
    /// Catppuccin Mocha theme - rich, colorful dark theme
    /// https://github.com/catppuccin/catppuccin
    pub fn catppuccin_mocha() -> Self {
        Self {
            // Base
            bg: Color::Rgb(30, 30, 46),        // Base
            fg: Color::Rgb(205, 214, 244),     // Text
            fg_dim: Color::Rgb(147, 153, 178), // Subtext0

            // Accents
            accent_primary: Color::Rgb(137, 180, 250),   // Blue
            accent_secondary: Color::Rgb(180, 190, 254), // Lavender
            accent_tertiary: Color::Rgb(250, 179, 135),  // Peach

            // Semantic
            success: Color::Rgb(166, 227, 161), // Green
            warning: Color::Rgb(249, 226, 175), // Yellow
            error: Color::Rgb(243, 139, 168),   // Red
            info: Color::Rgb(137, 180, 250),    // Blue

            // Status
            status_pending: Color::Rgb(147, 153, 178),   // Subtext0 (gray)
            status_launching: Color::Rgb(249, 226, 175), // Yellow
            status_running: Color::Rgb(137, 180, 250),   // Blue
            status_complete: Color::Rgb(166, 227, 161),  // Green
            status_failed: Color::Rgb(243, 139, 168),    // Red

            // Log levels
            log_trace: Color::Rgb(108, 112, 134),  // Overlay0 (dim)
            log_debug: Color::Rgb(148, 226, 213),  // Teal
            log_info: Color::Rgb(137, 180, 250),   // Blue
            log_warn: Color::Rgb(249, 226, 175),   // Yellow
            log_error: Color::Rgb(243, 139, 168),  // Red

            // UI elements
            border_focused: Color::Rgb(180, 190, 254),  // Lavender
            border_unfocused: Color::Rgb(69, 71, 90),   // Surface1
            selection_bg: Color::Rgb(69, 71, 90),       // Surface1
            highlight: Color::Rgb(203, 166, 247),       // Mauve
            header_bg: Color::Rgb(49, 50, 68),          // Surface0
            scrollbar_track: Color::Rgb(49, 50, 68),    // Surface0
            scrollbar_thumb: Color::Rgb(137, 180, 250), // Blue
        }
    }

    /// Get the style for a status
    pub fn status_style(&self, status: InstanceStatus) -> Style {
        Style::default().fg(self.status_color(status))
    }

    /// Get the color for a status
    pub fn status_color(&self, status: InstanceStatus) -> Color {
        match status {
            InstanceStatus::Pending => self.status_pending,
            InstanceStatus::Launching => self.status_launching,
            InstanceStatus::Running => self.status_running,
            InstanceStatus::Complete => self.status_complete,
            InstanceStatus::Failed => self.status_failed,
        }
    }

    /// Style for focused blocks
    pub fn block_focused(&self) -> Style {
        Style::default().fg(self.border_focused)
    }

    /// Style for unfocused blocks
    pub fn block_unfocused(&self) -> Style {
        Style::default().fg(self.border_unfocused)
    }

    /// Style for table headers
    pub fn table_header(&self) -> Style {
        Style::default()
            .fg(self.accent_tertiary)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for selected/highlighted items
    pub fn selection(&self) -> Style {
        Style::default().bg(self.selection_bg)
    }

    /// Style for dim/secondary text
    pub fn dim(&self) -> Style {
        Style::default().fg(self.fg_dim)
    }

    /// Style for primary text
    pub fn text(&self) -> Style {
        Style::default().fg(self.fg)
    }

    /// Style for bold text
    pub fn bold(&self) -> Style {
        Style::default()
            .fg(self.fg)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for success text
    pub fn success_style(&self) -> Style {
        Style::default().fg(self.success)
    }

    /// Style for warning text
    pub fn warning_style(&self) -> Style {
        Style::default().fg(self.warning)
    }

    /// Style for error text
    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error)
    }

    /// Style for info text
    pub fn info_style(&self) -> Style {
        Style::default().fg(self.info)
    }

    /// Header bar style based on app state
    pub fn header_style(&self, is_complete: bool, is_failed: bool, is_cleanup: bool) -> Style {
        // Use dark text on colored backgrounds for good contrast
        let dark_text = Color::Rgb(30, 30, 46); // Base color
        if is_failed {
            Style::default().fg(self.fg).bg(self.error)
        } else if is_cleanup {
            Style::default().fg(dark_text).bg(self.warning)
        } else if is_complete {
            Style::default().fg(dark_text).bg(self.success)
        } else {
            Style::default().fg(dark_text).bg(self.accent_primary)
        }
    }

    /// Keyboard shortcut badge style
    pub fn key_badge(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(30, 30, 46))
            .bg(self.fg_dim)
    }

    /// Scrollbar thumb style
    pub fn scrollbar_thumb_style(&self) -> Style {
        Style::default().fg(self.scrollbar_thumb)
    }

    /// Scrollbar track style
    pub fn scrollbar_track_style(&self) -> Style {
        Style::default().fg(self.scrollbar_track)
    }
}

/// Get the global theme instance
pub fn theme() -> &'static Theme {
    static THEME: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
    THEME.get_or_init(Theme::default)
}
