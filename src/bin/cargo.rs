#![feature(collections, core, io, path, env)]

extern crate "git2-curl" as git2_curl;
extern crate "rustc-serialize" as rustc_serialize;
extern crate cargo;
extern crate env_logger;
#[macro_use] extern crate log;

use std::collections::BTreeSet;
use std::env;
use std::old_io::fs::{self, PathExtensions};
use std::old_io::process::{Command,InheritFd,ExitStatus,ExitSignal};
use std::old_io;

use cargo::{execute_main_without_stdin, handle_error, shell};
use cargo::core::MultiShell;
use cargo::util::{CliError, CliResult, lev_distance, Config};

#[derive(RustcDecodable)]
struct Flags {
    flag_list: bool,
    flag_verbose: bool,
    arg_command: String,
    arg_args: Vec<String>,
}

const USAGE: &'static str = "
Rust's package manager

Usage:
    cargo <command> [<args>...]
    cargo [options]

Options:
    -h, --help       Display this message
    -V, --version    Print version info and exit
    --list           List installed commands
    -v, --verbose    Use verbose output

Some common cargo commands are:
    build       Compile the current project
    clean       Remove the target directory
    doc         Build this project's and its dependencies' documentation
    new         Create a new cargo project
    run         Build and execute src/main.rs
    test        Run the tests
    bench       Run the benchmarks
    update      Update dependencies listed in Cargo.lock

See 'cargo help <command>' for more information on a specific command.
";

fn main() {
    env_logger::init().unwrap();
    execute_main_without_stdin(execute, true, USAGE)
}

macro_rules! each_subcommand{ ($mac:ident) => ({
    $mac!(bench);
    $mac!(build);
    $mac!(clean);
    $mac!(doc);
    $mac!(fetch);
    $mac!(generate_lockfile);
    $mac!(git_checkout);
    $mac!(help);
    $mac!(locate_project);
    $mac!(login);
    $mac!(new);
    $mac!(owner);
    $mac!(package);
    $mac!(pkgid);
    $mac!(publish);
    $mac!(read_manifest);
    $mac!(run);
    $mac!(search);
    $mac!(test);
    $mac!(update);
    $mac!(verify_project);
    $mac!(version);
    $mac!(yank);
}) }

/**
  The top-level `cargo` command handles configuration and project location
  because they are fundamental (and intertwined). Other commands can rely
  on this top-level information.
*/
fn execute(flags: Flags, config: &Config) -> CliResult<Option<()>> {
    config.shell().set_verbose(flags.flag_verbose);

    init_git_transports(config);

    if flags.flag_list {
        println!("Installed Commands:");
        for command in list_commands().into_iter() {
            println!("    {}", command);
        };
        return Ok(None)
    }

    let (mut args, command) = match &flags.arg_command[] {
        "" | "help" if flags.arg_args.len() == 0 => {
            config.shell().set_verbose(true);
            let args = &["foo".to_string(), "-h".to_string()];
            let r = cargo::call_main_without_stdin(execute, config, USAGE, args,
                                                   false);
            cargo::process_executed(r, &mut **config.shell());
            return Ok(None)
        }
        "help" if flags.arg_args[0] == "-h" ||
                  flags.arg_args[0] == "--help" =>
            (flags.arg_args, "help"),
        "help" => (vec!["-h".to_string()], &flags.arg_args[0][]),
        s => (flags.arg_args.clone(), s),
    };
    args.insert(0, command.to_string());
    args.insert(0, "foo".to_string());

    macro_rules! cmd{ ($name:ident) => (
        if command == stringify!($name).replace("_", "-") {
            mod $name;
            config.shell().set_verbose(true);
            let r = cargo::call_main_without_stdin($name::execute, config,
                                                   $name::USAGE,
                                                   &args,
                                                   false);
            cargo::process_executed(r, &mut **config.shell());
            return Ok(None)
        }
    ) }
    each_subcommand!(cmd);

    execute_subcommand(&command, &args, &mut config.shell());
    Ok(None)
}

fn find_closest(cmd: &str) -> Option<String> {
    match list_commands().iter()
                            // doing it this way (instead of just .min_by(|c|
                            // c.lev_distance(cmd))) allows us to only make
                            // suggestions that have an edit distance of
                            // 3 or less
                            .map(|c| (lev_distance(&c, cmd), c))
                            .filter(|&(d, _): &(usize, &String)| d < 4)
                            .min_by(|&(d, _)| d) {
        Some((_, c)) => {
            Some(c.to_string())
        },
        None => None
    }
}

fn execute_subcommand(cmd: &str, args: &[String], shell: &mut MultiShell) {
    let command = match find_command(cmd) {
        Some(command) => command,
        None => {
            let msg = match find_closest(cmd) {
                Some(closest) => format!("No such subcommand\n\n\t\
                                          Did you mean `{}`?\n", closest),
                None => "No such subcommand".to_string()
            };
            return handle_error(CliError::new(&msg, 127), shell)
        }
    };
    let status = Command::new(command)
                         .args(args)
                         .stdin(InheritFd(0))
                         .stdout(InheritFd(1))
                         .stderr(InheritFd(2))
                         .status();

    match status {
        Ok(ExitStatus(0)) => (),
        Ok(ExitStatus(i)) => {
            handle_error(CliError::new("", i as i32), shell)
        }
        Ok(ExitSignal(i)) => {
            let msg = format!("subcommand failed with signal: {}", i);
            handle_error(CliError::new(&msg, i as i32), shell)
        }
        Err(old_io::IoError{kind, ..}) if kind == old_io::FileNotFound =>
            handle_error(CliError::new("No such subcommand", 127), shell),
        Err(err) => handle_error(
            CliError::new(
                &format!("Subcommand failed to run: {}", err), 127),
            shell)
    }
}

/// List all runnable commands. find_command should always succeed
/// if given one of returned command.
fn list_commands() -> BTreeSet<String> {
    let command_prefix = "cargo-";
    let mut commands = BTreeSet::new();
    for dir in list_command_directory().iter() {
        let entries = match fs::readdir(dir) {
            Ok(entries) => entries,
            _ => continue
        };
        for entry in entries.iter() {
            let filename = match entry.filename_str() {
                Some(filename) => filename,
                _ => continue
            };
            if filename.starts_with(command_prefix) &&
                    filename.ends_with(env::consts::EXE_SUFFIX) &&
                    is_executable(entry) {
                let command = &filename[
                    command_prefix.len()..
                    filename.len() - env::consts::EXE_SUFFIX.len()];
                commands.insert(String::from_str(command));
            }
        }
    }

    macro_rules! add_cmd{ ($cmd:ident) => ({
        commands.insert(stringify!($cmd).replace("_", "-"));
    }) }
    each_subcommand!(add_cmd);
    commands
}

fn is_executable(path: &Path) -> bool {
    match fs::stat(path) {
        Ok(old_io::FileStat{ kind: old_io::FileType::RegularFile, perm, ..}) =>
            perm.contains(old_io::OTHER_EXECUTE),
        _ => false
    }
}

/// Get `Command` to run given command.
fn find_command(cmd: &str) -> Option<Path> {
    let command_exe = format!("cargo-{}{}", cmd, env::consts::EXE_SUFFIX);
    let dirs = list_command_directory();
    let mut command_paths = dirs.iter().map(|dir| dir.join(&command_exe));
    command_paths.find(|path| path.exists())
}

/// List candidate locations where subcommands might be installed.
fn list_command_directory() -> Vec<Path> {
    let mut dirs = vec![];
    if let Ok(mut path) = env::current_exe() {
        path.pop();
        dirs.push(path.join("../lib/cargo"));
        dirs.push(path);
    }
    if let Some(val) = env::var("PATH") {
        dirs.extend(env::split_paths(&val));
    }
    dirs
}

fn init_git_transports(config: &Config) {
    // Only use a custom transport if a proxy is configured, right now libgit2
    // doesn't support proxies and we have to use a custom transport in this
    // case. The custom transport, however, is not as well battle-tested.
    match cargo::ops::http_proxy(config) {
        Ok(Some(..)) => {}
        _ => return
    }

    let handle = match cargo::ops::http_handle(config) {
        Ok(handle) => handle,
        Err(..) => return,
    };

    // The unsafety of the registration function derives from two aspects:
    //
    // 1. This call must be synchronized with all other registration calls as
    //    well as construction of new transports.
    // 2. The argument is leaked.
    //
    // We're clear on point (1) because this is only called at the start of this
    // binary (we know what the state of the world looks like) and we're mostly
    // clear on point (2) because we'd only free it after everything is done
    // anyway
    unsafe {
        git2_curl::register(handle);
    }
}
