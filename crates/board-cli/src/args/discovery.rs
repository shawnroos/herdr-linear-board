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
        /// The herdr space id; defaults to `$HERDR_WORKSPACE_ID`.
        workspace_id: Option<String>,
    },
    /// One Linear issue in full: description, sub-issues, parent and relations,
    /// comments and history, as the board's issue page reads it.
    Issue {
        /// The issue's identifier, e.g. `WEB-3318`.
        issue: String,
    },
    /// herdr spaces and their Linear binding state.
    Space {
        #[command(subcommand)]
        sub: LinearSpaceCmd,
    },
    /// Linear projects you are a member of.
    Project {
        #[command(subcommand)]
        sub: LinearProjectCmd,
    },
    /// A Linear project's views.
    View {
        #[command(subcommand)]
        sub: LinearViewCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum LinearSpaceCmd {
    /// List every space with its binding state.
    List,
}

#[derive(Subcommand)]
pub(crate) enum LinearProjectCmd {
    /// List the Linear projects you are a member of.
    List,
}

#[derive(Subcommand)]
pub(crate) enum LinearViewCmd {
    /// List one Linear project's views.
    List {
        /// The Linear project id (from `board linear project list`).
        project_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCmd {
    /// List herdr sessions.
    List,
}
