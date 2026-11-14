pub use refact_runtime_api::cli_dispatch::*;

pub fn version_text() -> String {
    let mut lines = vec![format!("refact {}", env!("CARGO_PKG_VERSION"))];
    lines.extend(
        crate::http::routers::info::get_build_info()
            .into_iter()
            .map(|(key, value)| format!("{:>20} {}", key, value)),
    );
    lines.join("\n")
}
