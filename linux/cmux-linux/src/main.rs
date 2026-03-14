mod app;
mod capabilities;
mod config;
mod model;
mod session;
mod socket;
mod state;
mod terminal_host;

fn main() -> gtk::glib::ExitCode {
    app::run()
}
