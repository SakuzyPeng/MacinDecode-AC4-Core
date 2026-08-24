//! AC-4 OAMD 到 DAMF 试听探针的导出。

use crate::wire::CliError;
#[cfg(not(feature = "audio-decode"))]
use crate::wire::DiagnosticCode;
use crate::{ExportDamfArgs, ExportFullDamfArgs};

const COMMAND: &str = "export-damf";
const FULL_COMMAND: &str = "export-full-damf";

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run(_args: ExportDamfArgs) -> Result<String, CliError> {
    Err(CliError::new(
        COMMAND,
        DiagnosticCode::FeatureRequired,
        "export-damf requires rebuilding macinac4 with --features audio-decode",
    ))
}

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run_full(_args: ExportFullDamfArgs) -> Result<String, CliError> {
    Err(CliError::new(
        FULL_COMMAND,
        DiagnosticCode::FeatureRequired,
        "export-full-damf requires rebuilding macinac4 with --features audio-decode",
    ))
}

#[cfg(feature = "audio-decode")]
mod enabled;

#[cfg(feature = "audio-decode")]
pub(crate) fn run(args: ExportDamfArgs) -> Result<String, CliError> {
    enabled::run(args)
}

#[cfg(feature = "audio-decode")]
pub(crate) fn run_full(args: ExportFullDamfArgs) -> Result<String, CliError> {
    enabled::run_full(args)
}
