//! Import commands. The daemon reads the store and writes the rows; the CLI
//! only sends the request.

use anyhow::Result;
use board_core::client::BoardClient;
use board_core::protocol::LinearImportParams;

use crate::args::ImportCmd;
use crate::context::Ctx;
use crate::render::emit;

pub(crate) fn cmd_import(sub: ImportCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        ImportCmd::WorkStore { dry_run } => {
            let result = ctx
                .client()?
                .linear_import(&LinearImportParams { dry_run })?;
            emit(&result, json)
        }
    }
}
