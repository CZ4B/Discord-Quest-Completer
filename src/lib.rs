pub mod database;
pub mod game_icons;
pub mod process;
pub mod scheduler;
pub mod session;
pub mod updates;

pub const APP_NAME: &str = "DISCORD QUEST COMPLETER";
pub const VERSION: &str = env!("DQC_APP_VERSION");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
