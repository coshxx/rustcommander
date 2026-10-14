//! Everything about files that is not a file *operation*: listing directories, names,
//! archives as read-only directories, marking masks, change notifications and
//! temporary files.

pub mod archive;
pub mod listing;
pub mod mask;
pub mod names;
pub mod temp;
pub mod watch;
