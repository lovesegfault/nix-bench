//! Mouse event handling for the TUI application

use super::App;
use super::PanelFocus;

impl App {
    /// Handle a mouse click at the given position
    /// Returns true if the click was handled
    pub fn handle_mouse_click(&mut self, x: u16, y: u16) -> bool {
        // Check instance list area
        if let Some(area) = self.ui.instance_list_area {
            let content_x = area.x + 1;
            let content_y = area.y + 1;
            let content_width = area.width.saturating_sub(2);
            let content_height = area.height.saturating_sub(2);

            if x >= content_x
                && x < content_x + content_width
                && y >= content_y
                && y < content_y + content_height
            {
                // Account for list scroll offset when calculating clicked index
                let clicked_index = self.ui.list_scroll_offset + (y - content_y) as usize;

                if clicked_index < self.instances.order.len() {
                    self.instances.selected_index = clicked_index;
                    self.ui.focus = PanelFocus::InstanceList;
                    return true;
                }
            }
        }

        // Check build output area
        if let Some(area) = self.ui.build_output_area {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                self.ui.focus = PanelFocus::BuildOutput;
                return true;
            }
        }

        false
    }

    /// Handle mouse scroll at given position
    pub fn handle_mouse_scroll(&mut self, x: u16, y: u16, down: bool) {
        // Check if scroll is in build output area
        if let Some(area) = self.ui.build_output_area {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                if down {
                    self.scroll_down(3);
                } else {
                    self.scroll_up(3);
                }
                return;
            }
        }

        // Default: scroll instance list
        if down {
            self.select_next();
        } else {
            self.select_previous();
        }
    }
}
