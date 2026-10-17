// Settings › Pages and the welcome's: Feed and Notes on or off (crates/filo-local `pages`). The app reads them once
// at launch and keeps them (src/pages.rs); agents read them from the database for each turn.

use filo_local::pages::{self, Page, Pages};

use super::{Local, now_ms};

impl Local {
    /// The pages as saved (both on when the database can't say).
    pub fn pages(&self) -> Pages {
        self.db().map(pages::get).unwrap_or_default()
    }

    /// Turns `page` on or off (Feed's topics' routines pause and resume with it, and old topics get theirs:
    /// `feed_carry_over`), then the pages hear of it.
    pub fn set_page(&self, page: Page, on: bool) -> Result<(), String> {
        pages::set(self.db()?, &chrono::Local, page, on, now_ms())?;
        if page == Page::Feed && on {
            self.feed_carry_over();
        }
        self.changed();
        Ok(())
    }

    /// The first welcome on this Mac: the pages nobody chose start off, unless they have something in them.
    pub fn start_pages_off(&self) -> Pages {
        if let Err(error) = self.db().and_then(|db| pages::start_off(db, &chrono::Local, now_ms())) {
            eprintln!("filo pages: {error}");
        }
        self.changed();
        self.pages()
    }
}
