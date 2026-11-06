//! The command line: the arguments, subcommands and their options.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[allow(clippy::doc_markdown, reason = "these doc comments are the help text")]
#[derive(Parser, Debug)]
#[command(
    name = "tamis",
    about = "A spam filter and Sieve mail sorter for remote mailboxes",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
pub(super) struct Cli {
    /// Print the version
    #[arg(short = 'V', long = "version", global = false)]
    pub(super) version: bool,
    /// Log each IMAP exchange
    #[arg(short, long, global = true)]
    pub(super) verbose: bool,
    /// Colour output: auto (a terminal, unless NO_COLOR is set), always or never
    #[arg(long, global = true, default_value = "auto")]
    pub(super) color: String,
    /// Configuration file
    #[arg(short, long, global = true)]
    pub(super) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(super) command: Option<Command>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ProbeArgs {
    /// IMAP server, e.g. imap.mail.me.com for iCloud
    #[arg(long)]
    pub(super) host: Option<String>,
    /// IMAP port (implicit TLS)
    #[arg(long)]
    pub(super) port: Option<u16>,
    /// Login name, usually the full address
    #[arg(long)]
    pub(super) user: Option<String>,
    /// Also create, fill and delete scratch folders
    #[arg(long)]
    pub(super) write: bool,
    /// Print the report as JSON
    #[arg(long)]
    pub(super) json: bool,
    /// How long to wait for an IDLE push
    #[arg(long, default_value = "30s")]
    pub(super) idle_wait: String,
    /// Connect without TLS (local test servers only)
    #[arg(long)]
    pub(super) plaintext: bool,
}

#[derive(Subcommand, Debug)]
pub(super) enum Command {
    /// Write a starter configuration file
    #[command(
        long_about = "Write a configuration file naming the account. Every other setting has a\ndefault. It refuses to overwrite an existing file."
    )]
    Init {
        /// IMAP server, e.g. imap.mail.me.com for iCloud
        #[arg(long)]
        host: String,
        /// Login name, usually the full address
        #[arg(long)]
        user: String,
    },
    /// Check which IMAP features a server supports
    #[command(
        long_about = "Check which IMAP features a server supports, and list its folders.\n\n--host, --user and --port default to the account in the configuration file.\nThe password (an app-specific one for iCloud) comes from the same places as\nfor `tamis run`."
    )]
    Probe(ProbeArgs),
    /// Check the account password and keep it in the system keychain
    #[command(
        long_about = "Ask for the account password (an app-specific one for iCloud), check it\nagainst the server, and store it in the system keychain: the macOS Keychain,\nthe Secret Service (GNOME Keyring, KWallet) or the Windows Credential Manager."
    )]
    Login,
    /// Remove the account password from the system keychain
    Logout,
    /// Seed the whitelist and the filter from existing mail
    #[command(
        long_about = "Seed the whitelist from Sent recipients and the filter from Junk (as spam) and\nold read inbox mail (as good), then start watching from current mail."
    )]
    Bootstrap(BootstrapArgs),
    /// Judge new mail, then keep watching
    #[command(
        long_about = "Train from the training folders and judge new mail in the watched folders,\nevery policy.poll_interval until interrupted. In shadow mode decisions are only\nrecorded; see them with 'tamis log'."
    )]
    Run {
        /// Make a single pass and exit
        #[arg(long)]
        once: bool,
    },
    /// Reverse decisions tamis applied
    #[command(
        long_about = "Reverse decisions tamis applied, by the IDs 'tamis log' shows. Mail filed\nto Junk is moved back, learnt as good and its sender whitelisted; an\nuncertain flag is cleared."
    )]
    Undo {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Teach tamis the right answer for decisions it only recorded
    #[command(
        long_about = "Teach tamis the right answer for decisions it recorded without acting on, or\nflagged uncertain, by the IDs 'tamis log' shows. With no flag it learns the opposite\nof the verdict. The mail stays where it is, in its folder or in Junk, except an uncertain\none: its flag is cleared, and it moves to Junk if learnt as spam. A\ncorrection can be learnt again to change it. Use 'tamis undo' for other\ndecisions that were applied."
    )]
    Learn {
        #[arg(required = true)]
        ids: Vec<i64>,
        /// Learn as good
        #[arg(long, conflicts_with = "spam")]
        good: bool,
        /// Learn as spam
        #[arg(long)]
        spam: bool,
    },
    /// Show recent decisions
    #[command(
        long_about = "Show recent decisions, newest first. In shadow mode, actions read\n\"would ...\": what live mode would have done."
    )]
    Log {
        /// How many decisions to show
        #[arg(short = 'n', long = "number", default_value_t = 20)]
        number: i64,
    },
    /// Show the configuration and check the rules and that the filter is in good shape
    #[command(
        long_about = "Show the configuration, check that it loads and the filing rules compile, that the filter\nhas learnt from both kinds of mail, how it has been deciding lately, and whether\nit is still deciding. Exits non-zero when a check fails."
    )]
    Doctor {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the checks to set up on a monitoring service such as healthchecks.io
    #[command(
        long_about = "Work out, from poll_interval and monitor.fail_after, the period and grace time of\nthe two checks `tamis run` pings when [monitor] is configured, and how soon each\nkind of trouble would alert."
    )]
    Monitor {
        /// Print the settings as JSON
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<MonitorAction>,
    },
    /// Manage passkeys for the web interface
    #[command(
        long_about = "The web interface (the [web] config section, served by `tamis run`) signs in\nwith passkeys. Enroll the first one with `tamis web enroll`."
    )]
    Web {
        #[command(subcommand)]
        action: WebAction,
    },
    /// View and edit the whitelist
    #[command(
        long_about = "The whitelist holds senders tamis trusts. Recipients you write to are\nadded automatically; these commands add and remove addresses by hand."
    )]
    Whitelist {
        #[command(subcommand)]
        action: WhitelistAction,
    },
    /// View, validate and try the filing rules
    #[command(
        long_about = "Good mail is filed into folders by a Sieve script (RFC 5228) named by\nrules.script. Only fileinto matters: tamis moves mail over IMAP."
    )]
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Print the version
    Version,
}

/// What `bootstrap` learns from.
#[derive(clap::Args, Debug)]
pub(crate) struct BootstrapArgs {
    /// Recent sent messages to whitelist recipients from
    #[arg(long)]
    pub sent: Option<usize>,
    /// FOLDER=N: train the latest N messages as spam (default: junk folder=500)
    #[arg(short = 's', long = "spam")]
    pub spam: Vec<String>,
    /// FOLDER=N: train the latest N read messages as good (default: first watched folder=500)
    #[arg(short = 'g', long = "good")]
    pub good: Vec<String>,
    /// Only train read mail at least this old as good
    #[arg(long, default_value = "168h")]
    pub good_min_age: String,
    /// Learn already-learnt mail again with the current tokenizer; leaves the whitelist and watch position alone
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Subcommand, Debug)]
pub(super) enum MonitorAction {
    /// Send one ping to each configured check, to see that the URLs work
    Test,
}

#[derive(Subcommand, Debug)]
pub(super) enum WebAction {
    /// Print a one-time link to enroll a passkey
    Enroll,
    /// List enrolled passkeys
    Passkeys,
    /// Show the certificate to install on your devices
    #[command(
        long_about = "Show the certificate the web interface serves HTTPS with, making a self-signed\none first if web.self_signed is set. Browsers only accept passkeys from a\ncertificate they trust, so install it on each device. --pem prints the\ncertificate itself, never the key."
    )]
    Cert {
        /// Print the certificate as PEM, for copying to a device
        #[arg(long)]
        pem: bool,
    },
    /// Remove enrolled passkeys
    Revoke {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
}

#[derive(Subcommand, Debug)]
pub(super) enum WhitelistAction {
    /// List whitelisted addresses
    List,
    /// Whitelist addresses
    Add {
        #[arg(required = true)]
        addresses: Vec<String>,
    },
    /// Remove addresses from the whitelist
    #[command(
        visible_alias = "remove",
        long_about = "Remove the given addresses, or with --matching every address or source that\ncontains TEXT. --matching lists what it would remove and does nothing more\nuntil you repeat it with --yes."
    )]
    Rm {
        #[arg(required_unless_present = "matching", conflicts_with = "matching")]
        addresses: Vec<String>,
        /// Remove every address or source containing this text
        #[arg(long)]
        matching: Option<String>,
        /// With --matching, really remove them
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(super) enum RulesAction {
    /// Print the configured rules script
    Show,
    /// Validate a Sieve script (default: rules.script)
    #[command(
        long_about = "Compile the script and report any error. With --folders, also connect to the\nmailbox and fail if a folder the script files into doesn't exist."
    )]
    Check {
        file: Option<PathBuf>,
        /// Also check the folders the script files into exist in the mailbox
        #[arg(long)]
        folders: bool,
    },
    /// Show where the rules would file messages
    #[command(
        long_about = "Run the script on each message and print where it would be filed. With\n--expect-header, compare that with the folder the message names in that header\n(`stay` for a message no rule files) and exit 1 if any differ. --script tries a\nscript other than rules.script, without needing a configuration file."
    )]
    Test {
        #[arg(required = true)]
        messages: Vec<PathBuf>,
        /// Try this script instead of rules.script
        #[arg(long)]
        script: Option<PathBuf>,
        /// Header naming the folder each message should be filed into, or `stay`
        #[arg(long, value_name = "HEADER")]
        expect_header: Option<String>,
    },
}
