use clap::error::ErrorKind;
use is_terminal::IsTerminal;
use sn::cli::{
    AppSub, AtfSub, AttachmentSub, AuthSub, CatalogSub, ChangeSub, Cli, CmdbSub, Command,
    IdentifySub, ImportSub, SchemaSub, ScoresSub, TableSub, UpdateSetSub, UserSub, VariablesSub,
};
use sn::error::Result;
use sn::output::emit_error;
use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match sn::cli::parse() {
        Ok(cli) => cli,
        Err(err) => return handle_clap_error(err),
    };
    sn::observability::set_level(cli.global.verbose);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(sn::error::Error::BrokenPipe) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = emit_error(io::stderr().lock(), &err);
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

/// Keep clap errors inside the machine contract: usage mistakes exit 1 (not
/// clap's default 2, which this CLI reserves for API errors) and produce the
/// JSON error envelope when stderr is not a terminal.
fn handle_clap_error(err: clap::Error) -> ExitCode {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = err.print();
            ExitCode::SUCCESS
        }
        _ => {
            let hint = subcommand_hint(&err);
            if io::stderr().is_terminal() {
                let _ = err.print();
                if let Some(hint) = hint {
                    eprintln!("{hint}");
                }
            } else {
                let mut message = err.render().to_string().trim_end().to_string();
                if let Some(hint) = hint {
                    message.push_str("\n\n");
                    message.push_str(&hint);
                }
                let _ = emit_error(io::stderr().lock(), &sn::error::Error::Usage(message));
            }
            ExitCode::from(1)
        }
    }
}

/// clap's "unrecognized subcommand 'sc_cat_item'" names neither what is valid
/// nor how to proceed — it only suggests when the token is a near-miss of a real
/// verb, which a table name never is. List the group's subcommands instead, so
/// a caller (or an agent reading the JSON error) can recover in one step.
fn subcommand_hint(err: &clap::Error) -> Option<String> {
    if err.kind() != ErrorKind::InvalidSubcommand {
        return None;
    }
    let (group, names) = sn::cli::valid_subcommands(err)?;
    Some(format!("  `sn {group}` subcommands: {}", names.join(", ")))
}

fn run(cli: Cli) -> Result<()> {
    let Cli {
        global, command, ..
    } = cli;
    match command {
        Command::Init(args) => sn::cli::init::run(&global, args),
        Command::Auth { sub } => match sub {
            AuthSub::Login => sn::cli::auth::login(&global),
            AuthSub::Logout => sn::cli::auth::logout(&global),
            AuthSub::Status => sn::cli::auth::status(&global),
            AuthSub::Refresh => sn::cli::auth::refresh(&global),
        },
        Command::Profile { sub } => sn::cli::profile::run(&global, sub),
        Command::Introspect => sn::cli::introspect::run(&global),
        Command::Table { sub } => match sub {
            TableSub::List(args) => sn::cli::table::list(&global, args),
            TableSub::Get(args) => sn::cli::table::get(&global, args),
            TableSub::Create(args) => sn::cli::table::create(&global, args),
            TableSub::Update(args) => sn::cli::table::update(&global, args),
            TableSub::Delete(args) => sn::cli::table::delete(&global, args),
        },
        Command::Journal(args) => sn::cli::journal::run(&global, args),
        Command::Variables { sub } => match sub {
            VariablesSub::Get(args) => sn::cli::variables::get(&global, args),
            VariablesSub::Set(args) => sn::cli::variables::set(&global, args),
        },
        Command::Watch { sub } => sn::cli::watch::run(&global, sub),
        Command::Schema { sub } => match sub {
            SchemaSub::Tables(args) => sn::cli::schema::tables(&global, args),
            SchemaSub::Columns(args) => sn::cli::schema::columns(&global, args),
            SchemaSub::Choices(args) => sn::cli::schema::choices(&global, args),
        },
        Command::Progress(args) => sn::cli::progress::run(&global, args),
        Command::App { sub } => match sub {
            AppSub::Install(args) => sn::cli::app::install(&global, args),
            AppSub::Publish(args) => sn::cli::app::publish(&global, args),
            AppSub::Rollback(args) => sn::cli::app::rollback(&global, args),
        },
        Command::UpdateSet { sub } => match sub {
            UpdateSetSub::Create(args) => sn::cli::update_set::create(&global, args),
            UpdateSetSub::Retrieve(args) => sn::cli::update_set::retrieve(&global, args),
            UpdateSetSub::Preview(args) => sn::cli::update_set::preview(&global, args),
            UpdateSetSub::Commit(args) => sn::cli::update_set::commit(&global, args),
            UpdateSetSub::CommitMultiple(args) => {
                sn::cli::update_set::commit_multiple(&global, args)
            }
            UpdateSetSub::BackOut(args) => sn::cli::update_set::back_out(&global, args),
        },
        Command::Atf { sub } => match sub {
            AtfSub::Run(args) => sn::cli::atf::run(&global, args),
            AtfSub::Results(args) => sn::cli::atf::results(&global, args),
        },
        Command::Aggregate(args) => sn::cli::aggregate::run(&global, args),
        Command::Scores { sub } => match sub {
            ScoresSub::List(args) => sn::cli::scores::list(&global, *args),
            ScoresSub::Favorite(args) => sn::cli::scores::favorite(&global, args),
            ScoresSub::Unfavorite(args) => sn::cli::scores::unfavorite(&global, args),
        },
        Command::Change { sub } => match sub {
            ChangeSub::List(args) => sn::cli::change::list(&global, args),
            ChangeSub::Get(args) => sn::cli::change::get(&global, args),
            ChangeSub::Create(args) => sn::cli::change::create(&global, args),
            ChangeSub::Update(args) => sn::cli::change::update(&global, args),
            ChangeSub::Delete(args) => sn::cli::change::delete(&global, args),
            ChangeSub::Nextstates(args) => sn::cli::change::nextstates(&global, args),
            ChangeSub::Approvals(args) => sn::cli::change::approvals(&global, args),
            ChangeSub::Risk(args) => sn::cli::change::risk(&global, args),
            ChangeSub::Schedule(args) => sn::cli::change::schedule(&global, args),
            ChangeSub::Task { sub } => sn::cli::change::task(&global, sub),
            ChangeSub::Ci { sub } => sn::cli::change::ci(&global, sub),
            ChangeSub::Conflict { sub } => sn::cli::change::conflict(&global, sub),
            ChangeSub::Models(args) => sn::cli::change::models(&global, args),
            ChangeSub::Templates(args) => sn::cli::change::templates(&global, args),
        },
        Command::Attachment { sub } => match sub {
            AttachmentSub::List(args) => sn::cli::attachment::list(&global, args),
            AttachmentSub::Get(args) => sn::cli::attachment::get(&global, args),
            AttachmentSub::Upload(args) => sn::cli::attachment::upload(&global, args),
            AttachmentSub::Download(args) => sn::cli::attachment::download(&global, args),
            AttachmentSub::Delete(args) => sn::cli::attachment::delete(&global, args),
        },
        Command::Cmdb { sub } => match sub {
            CmdbSub::List(args) => sn::cli::cmdb::list(&global, args),
            CmdbSub::Get(args) => sn::cli::cmdb::get(&global, args),
            CmdbSub::Create(args) => sn::cli::cmdb::create(&global, args),
            CmdbSub::Update(args) => sn::cli::cmdb::update(&global, args),
            CmdbSub::Meta(args) => sn::cli::cmdb::meta(&global, args),
            CmdbSub::Relation { sub } => sn::cli::cmdb::relation(&global, sub),
        },
        Command::Import { sub } => match sub {
            ImportSub::Create(args) => sn::cli::import::create(&global, args),
            ImportSub::Bulk(args) => sn::cli::import::bulk(&global, args),
            ImportSub::Get(args) => sn::cli::import::get(&global, args),
        },
        Command::Catalog { sub } => match sub {
            CatalogSub::List(args) => sn::cli::catalog::list(&global, args),
            CatalogSub::Get(args) => sn::cli::catalog::get(&global, args),
            CatalogSub::Categories(args) => sn::cli::catalog::categories(&global, args),
            CatalogSub::Category(args) => sn::cli::catalog::category(&global, args),
            CatalogSub::Items(args) => sn::cli::catalog::items(&global, args),
            CatalogSub::Item(args) => sn::cli::catalog::item(&global, args),
            CatalogSub::ItemVariables(args) => sn::cli::catalog::item_variables(&global, args),
            CatalogSub::Order(args) => sn::cli::catalog::order(&global, args),
            CatalogSub::AddToCart(args) => sn::cli::catalog::add_to_cart(&global, args),
            CatalogSub::Cart => sn::cli::catalog::cart(&global),
            CatalogSub::CartUpdate(args) => sn::cli::catalog::cart_update(&global, args),
            CatalogSub::CartRemove(args) => sn::cli::catalog::cart_remove(&global, args),
            CatalogSub::CartEmpty(args) => sn::cli::catalog::cart_empty(&global, args),
            CatalogSub::Checkout => sn::cli::catalog::checkout(&global),
            CatalogSub::SubmitOrder => sn::cli::catalog::submit_order(&global),
            CatalogSub::Wishlist => sn::cli::catalog::wishlist(&global),
        },
        Command::Identify { sub } => match sub {
            IdentifySub::CreateUpdate(args) => sn::cli::identify::create_update(&global, args),
            IdentifySub::Query(args) => sn::cli::identify::query(&global, args),
            IdentifySub::CreateUpdateEnhanced(args) => {
                sn::cli::identify::create_update_enhanced(&global, args)
            }
            IdentifySub::QueryEnhanced(args) => sn::cli::identify::query_enhanced(&global, args),
        },
        Command::User { sub } => match sub {
            UserSub::Me => sn::cli::user::me(&global),
        },
        Command::Ping => sn::cli::ping::run(&global),
        Command::Open(args) => sn::cli::open_record::run(&global, args),
        Command::Raw(args) => sn::cli::raw::run(&global, args),
        Command::Graphql(args) => sn::cli::graphql::run(&global, args),
        Command::Completion(args) => sn::cli::completion::run(args),
    }
}
