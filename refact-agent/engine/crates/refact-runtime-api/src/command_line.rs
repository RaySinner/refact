use structopt::StructOpt;

fn daemon_auth_token_from_env() -> Option<String> {
    std::env::var("REFACT_DAEMON_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
}

#[derive(Debug, StructOpt, Clone)]
pub struct CommandLine {
    #[structopt(
        long,
        default_value = "pong",
        help = "A message to return in /v1/ping, useful to verify you're talking to the same process that you've started."
    )]
    pub ping_message: String,
    #[structopt(
        long,
        help = "Send logs to stderr, as opposed to ~/.cache/refact/logs, so it's easier to debug."
    )]
    pub logs_stderr: bool,
    #[structopt(long, default_value = "", help = "Send logs to a file.")]
    pub logs_to_file: String,
    #[structopt(
        long,
        help = "Trust self-signed SSL certificates, when connecting to an inference server."
    )]
    pub insecure: bool,

    #[structopt(
        long,
        short = "p",
        default_value = "0",
        help = "Bind <host>:<port> to listen for HTTP requests, such as /v1/code-completion, /v1/chat, /v1/caps."
    )]
    pub http_port: u16,
    #[structopt(
        long,
        help = "Host/address for the HTTP server. Defaults to 127.0.0.1, or 0.0.0.0 with --inside-container. Use 0.0.0.0 only on trusted networks."
    )]
    pub http_host: Option<String>,
    #[structopt(
        long,
        default_value = "0",
        help = "Bind 127.0.0.1:<port> and act as an LSP server. This is compatible with having an HTTP server at the same time."
    )]
    pub lsp_port: u16,
    #[structopt(
        long,
        default_value = "0",
        help = "Act as an LSP server, use stdin stdout for communication. This is compatible with having an HTTP server at the same time. But it's not compatible with LSP port."
    )]
    pub lsp_stdin_stdout: u16,

    #[structopt(
        long,
        short = "v",
        help = "Makes DEBUG log level visible, instead of the default INFO."
    )]
    pub verbose: bool,
    #[structopt(
        long,
        help = "Use AST, for it to start working, give it a jsonl files list or LSP workspace folders."
    )]
    pub ast: bool,
    // #[structopt(long, help="Use AST light mode, could be useful for large projects and little memory. Less information gets stored.")]
    // pub ast_light_mode: bool,
    #[structopt(
        long,
        default_value = "50000",
        help = "Maximum files for AST index, to avoid OOM on large projects."
    )]
    pub ast_max_files: usize,
    #[structopt(
        long,
        default_value = "",
        help = "Give it a path for AST database to make it permanent, if there is the database already, process starts without parsing all the files (careful). This quick start is helpful for automated solution search."
    )]
    pub ast_permanent: String,
    #[structopt(long, help = "Wait until AST is ready before responding requests.")]
    pub wait_ast: bool,

    #[structopt(
        long,
        help = "Use vector database. Give it LSP workspace folders or a jsonl, it also needs an embedding model."
    )]
    pub vecdb: bool,
    #[structopt(
        long,
        default_value = "15000",
        help = "Maximum files count for VecDB index, to avoid OOM."
    )]
    pub vecdb_max_files: usize,
    #[structopt(long, default_value = "", help = "Set VecDB storage path manually.")]
    pub vecdb_force_path: String,
    #[structopt(long, help = "Wait until VecDB is ready before responding requests.")]
    pub wait_vecdb: bool,

    #[structopt(
        long,
        short = "f",
        default_value = "",
        help = "A path to jsonl file with {\"path\": ...} on each line, files will immediately go to VecDB and AST."
    )]
    pub files_jsonl_path: String,
    #[structopt(
        long,
        short = "w",
        default_value = "",
        help = "Workspace folder to find all the files. An LSP or HTTP request can override this later."
    )]
    pub workspace_folder: String,
    #[structopt(
        long,
        help = "Start even when another Refact engine already holds the lease for a workspace folder. Both processes will then index and write the same workspace state, which is normally a bug."
    )]
    pub allow_shared_workspace: bool,

    #[structopt(
        long,
        help = "create yaml configs, like customization.yaml, privacy.yaml and exit."
    )]
    pub only_create_yaml_configs: bool,
    #[structopt(
        long,
        help = "Print combined project registry (modes, subagents, toolbox commands, code lens)."
    )]
    pub print_customization: bool,

    #[structopt(long, help = "Enable experimental features, such as new integrations.")]
    pub experimental: bool,

    #[structopt(
        long,
        help = "A way to tell this binary it can run more tools without confirmation."
    )]
    pub inside_container: bool,

    #[structopt(
        long,
        default_value = "",
        help = "Specify the integrations.yaml, this also disables the global integrations.d"
    )]
    pub integrations_yaml: String,

    #[structopt(
        long,
        default_value = "",
        help = "Specify the variables.yaml, disabling the global one"
    )]
    pub variables_yaml: String,
    #[structopt(
        long,
        default_value = "",
        help = "Specify the secrets.yaml, disabling the global one"
    )]
    pub secrets_yaml: String,
    #[structopt(
        long,
        default_value = "",
        help = "Specify the indexing.yaml, replacing the global one"
    )]
    pub indexing_yaml: String,
    #[structopt(
        long,
        default_value = "",
        help = "Specify the privacy.yaml, replacing the global one"
    )]
    pub privacy_yaml: String,
    #[structopt(long, help = "Disable the scheduler runner and cron scheduling tools.")]
    pub no_scheduler: bool,
    #[structopt(
        long,
        default_value = "",
        help = "Daemon endpoint for managed workers."
    )]
    pub daemon_endpoint: String,
    #[structopt(skip = daemon_auth_token_from_env())]
    pub daemon_auth_token: Option<String>,
    #[structopt(
        long,
        help = "Watch stdin and shut down gracefully when it reaches EOF. For IDE plugins that spawn the engine as an attached child process, so the engine exits when the parent dies. Incompatible with --lsp-stdin-stdout, which owns stdin; the flag is ignored there."
    )]
    pub watch_parent_stdin: bool,
    #[structopt(
        long,
        help = "Do not advertise the HTTP server over mDNS. IDE plugins that spawn one engine per window pass this, otherwise every window publishes a conflicting _refact-lsp._tcp record."
    )]
    pub no_mdns: bool,

    #[structopt(
        long,
        default_value = "",
        help = "Daemon project id for managed workers."
    )]
    pub project_id: String,
}

impl CommandLine {
    pub fn with_daemon_auth_token_from_env(mut self) -> Self {
        self.daemon_auth_token = self.daemon_auth_token.or_else(daemon_auth_token_from_env);
        self
    }
}
