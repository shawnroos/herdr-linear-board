//! Import subcommands.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ImportCmd {
    /// Copy the work plugin's store (~/.claude/work as the daemon sees it,
    /// or HERDR_LINEAR_STORE_DIR in the daemon's environment) into the board.
    /// Insert-only: rows the board already holds are skipped, so it is safe
    /// to run again.
    WorkStore {
        /// List what would be imported and skipped without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}
