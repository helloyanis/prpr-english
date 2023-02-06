use super::{Page, SharedState, SIDE_PADDING};
use anyhow::Result;
use macroquad::prelude::Touch;
use prpr::ui::{Scroll, Ui};

pub struct AboutPage {
    scroll: Scroll,
    text: String,
}

impl AboutPage {
    pub fn new() -> Self {
        Self {
            scroll: Scroll::new(),
            text: format!(
                r"prpr-client v{}
prpr is a Phigros simulator designed to provide a unified platform for homemade play. Please consciously abide by the relevant requirements of the community, do not use PRPR maliciously, and do not arbitrarily produce or disseminate low-quality works.

The default Material Skins used in this software (including note materials and percussion effects) are derived from @MisaLiu's phi-chart-render (https://github.com/MisaLiu/phi-chart-render), signed under the CC BY-NC 4.0 license (https://creativecommons.org/licenses/by-nc/4.0/). During the development of this software, these materials were resized and compressed for use.

prpr is open source software under the GNU General Public License v3.0.
Test Group：660488396
GitHub: https://github.com/Mivik/prpr
English version GitHub: https://github.com/helloyanis/prpr-english",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}

impl Page for AboutPage {
    fn label(&self) -> &'static str {
        "About"
    }

    fn update(&mut self, _focus: bool, state: &mut SharedState) -> Result<()> {
        self.scroll.update(state.t);
        Ok(())
    }
    fn touch(&mut self, touch: &Touch, state: &mut SharedState) -> Result<bool> {
        if self.scroll.touch(touch, state.t) {
            return Ok(true);
        }
        Ok(false)
    }
    fn render(&mut self, ui: &mut Ui, state: &mut SharedState) -> Result<()> {
        ui.dx(0.02);
        ui.dy(0.01);
        self.scroll.size(state.content_size);
        self.scroll.render(ui, |ui| {
            let r = ui
                .text(&self.text)
                .multiline()
                .max_width((1. - SIDE_PADDING) * 2. - 0.02)
                .size(0.5)
                .draw();
            (r.w, r.h)
        });
        Ok(())
    }
}
