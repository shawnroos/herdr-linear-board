//! Read-only discovery subcommands: harness capabilities, spaces, sessions,
//! the Linear-mode snapshot.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum HarnessCmd {
    /// List every available harness.
    List,
    /// List known models and the efforts each accepts.
    Models {
        #[arg(default_value = "pi")]
        harness: String,
    },
    /// Show the efforts a model accepts.
    Efforts {
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        model: String,
    },
    /// List permission modes a harness understands.
    Permissions {
        #[arg(default_value = "pi")]
        harness: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SpaceCmd {
    /// List run spaces (herdr workspaces) in a session.
    List {
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum LinearCmd {
    /// Print the work plugin's snapshot of one herdr space (the Linear-mode
    /// board's read), with live pane status attached.
    Snapshot {
        /// The herdr space id (`HERDR_WORKSPACE_ID` inside a pane).
        workspace_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCmd {
    /// List herdr sessions.
    List,
}
