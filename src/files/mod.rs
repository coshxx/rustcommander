//! Everything about files that is not a file *operation*: listing directories, file search,
//! folder sizes, names, sort order, archives as read-only directories, folder kinds and what
//! they allow, marking masks, change
//! notifications and temporary files.

pub mod archive;
pub mod dirsize;
pub mod find;
pub mod folder_kind;
pub mod listing;
pub mod location;
pub mod mask;
pub mod names;
pub mod sort;
pub mod temp;
pub mod watch;
