//! simple-whipper — a small CD ripper for the Linux desktop.
//!
//! libcdio-paranoia does the reading (through the `paranoia` crate), iced
//! draws the window.

mod app;
mod config;
mod encode;
mod rip;

fn main() -> iced::Result {
    iced::application(app::App::new, app::App::update, app::App::view)
        .title("Simple Whipper")
        .theme(app::App::theme)
        .window_size((760.0, 680.0))
        .run()
}
