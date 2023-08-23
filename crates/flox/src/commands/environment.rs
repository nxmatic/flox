use std::env::current_dir;
use std::fs::File;
use std::io::stdin;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use bpaf::{construct, Bpaf, Parser, ShellComp};
use flox_rust_sdk::flox::{EnvironmentName, Flox};
use flox_rust_sdk::models::environment::{Environment, Original, PathEnvironment};
use flox_rust_sdk::models::environment_ref;
use flox_rust_sdk::nix::arguments::eval::EvaluationArgs;
use flox_rust_sdk::nix::command::{Shell, StoreGc};
use flox_rust_sdk::nix::command_line::NixCommandLine;
use flox_rust_sdk::nix::Run;
use flox_rust_sdk::prelude::flox_package::FloxPackage;
use flox_types::constants::{DEFAULT_CHANNEL, LATEST_VERSION};
use itertools::Itertools;
use log::{error, info};

use crate::flox_forward;
use crate::utils::dialog::{Confirm, Dialog};
use crate::utils::resolve_environment_ref;

#[derive(Bpaf, Clone)]
pub struct EnvironmentArgs {
    #[bpaf(short, long, argument("SYSTEM"))]
    pub system: Option<String>,
}

pub type EnvironmentRef = String;

// impl EnvironmentCommands {
//     pub async fn handle(&self, flox: Flox) -> Result<()> {
//         match self {
//             EnvironmentCommands::List { .. } => subcommand_metric!("list"),
//             EnvironmentCommands::Envs => subcommand_metric!("envs"),
//             EnvironmentCommands::Activate { .. } => subcommand_metric!("activate"),
//             EnvironmentCommands::Init { .. } => subcommand_metric!("init"),
//             EnvironmentCommands::Destroy { .. } => subcommand_metric!("destroy"),
//             EnvironmentCommands::Edit { .. } => subcommand_metric!("edit"),
//             EnvironmentCommands::Export { .. } => subcommand_metric!("export"),
//             EnvironmentCommands::Generations { .. } => subcommand_metric!("generations"),
//             EnvironmentCommands::Git { .. } => subcommand_metric!("git"),
//             EnvironmentCommands::History { .. } => subcommand_metric!("history"),
//             EnvironmentCommands::Import { .. } => subcommand_metric!("import"),
//             EnvironmentCommands::Install { .. } => subcommand_metric!("install"),
//             EnvironmentCommands::Push { .. } => subcommand_metric!("push"),
//             EnvironmentCommands::Pull { .. } => subcommand_metric!("pull"),
//             EnvironmentCommands::Uninstall { .. } => subcommand_metric!("remove"),
//             EnvironmentCommands::Rollback { .. } => subcommand_metric!("rollback"),
//             EnvironmentCommands::SwitchGeneration { .. } => subcommand_metric!("switch"),
//             EnvironmentCommands::Upgrade { .. } => subcommand_metric!("upgrade"),
//             EnvironmentCommands::WipeHistory { .. } => subcommand_metric!("wipe-history"),
//         }

/// Edit declarative environment configuration
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Edit {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    /// Replace environment declaration with that in FILE
    #[bpaf(long, short, argument("FILE"))]
    file: Option<PathBuf>,
}
impl Edit {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let mut environment =
            resolve_environment(&flox, self.environment.as_deref(), "install").await?;
        let mut temporary_environment = environment.make_temporary().await?;

        let nix = flox.nix(Default::default());

        if let Some(file) = self.file {
            let mut file: Box<dyn std::io::Read + Send> = if file == Path::new("-") {
                Box::new(stdin())
            } else {
                Box::new(File::open(file).unwrap())
            };

            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            temporary_environment.update_manifest(&contents)?;
            temporary_environment.build(&nix, &flox.system).await?;
            return Ok(());
        }

        let editor = std::env::var("EDITOR").context("$EDITOR not set")?;
        if !Dialog::can_prompt() {
            bail!("Can't open editor in non interactive context");
        }

        loop {
            let path = temporary_environment.manifest_path();
            let mut command = Command::new(&editor);
            command.arg(&path);

            let child = command.spawn().context("editor command failed")?;
            let _ = child.wait_with_output().context("editor command failed")?;

            let contents = std::fs::read_to_string(path)?;
            temporary_environment.update_manifest(&contents)?;
            let build_result = temporary_environment.build(&nix, &flox.system).await;

            match build_result {
                Ok(_) => {
                    break;
                },
                Err(e) => {
                    error!("Environment invalid, building resulted in an error: {e}");
                    let again = Dialog {
                        message: "Continue editing?",
                        help_message: Default::default(),
                        typed: Confirm {
                            default: Some(true),
                        },
                    }
                    .prompt()
                    .await?;
                    if !again {
                        return Ok(());
                    }
                },
            };
        }
        environment
            .replace_with(temporary_environment)
            .context("Failed applying environment changes")?;
        Ok(())
    }
}

/// Delete an environment
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Delete {
    #[bpaf(short, long)]
    force: bool,

    #[bpaf(short, long)]
    origin: bool,

    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}

impl Delete {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let environment =
            resolve_environment(&flox, self.environment.as_deref(), "install").await?;

        environment
            .delete()
            .context("Failed to delete environment")?;
        Ok(())
    }
}

/// Activate environment
///
/// * in current shell: eval "$(flox activate)"
///
/// * in subshell: flox activate
///
/// * for command: flox activate -- <command> <args>
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Activate {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Vec<EnvironmentRef>,

    #[bpaf(external(activate_run_args))]
    arguments: Option<(String, Vec<String>)>,
}

impl Activate {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let environment = self.environment.first().map(|e| e.as_ref());
        let environment = resolve_environment(&flox, environment, "install").await?;

        let command = Shell {
            eval: EvaluationArgs {
                impure: true.into(),
                ..Default::default()
            },
            installables: [environment.flake_attribute(&flox.system).into()].into(),
            ..Default::default()
        };

        let nix = flox.nix(Default::default());
        command.run(&nix, &Default::default()).await?;
        Ok(())
    }
}

/// Create an environment in the current directory
#[derive(Bpaf, Clone)]
#[bpaf(command, long("create"))]
pub struct Init {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}
impl Init {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let current_dir = std::env::current_dir().unwrap();
        let home_dir = dirs::home_dir().unwrap();

        let name = if let Some(name) = self.environment.clone() {
            name
        } else if current_dir == home_dir {
            "default".to_string()
        } else {
            current_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .context("Can't init in root")?
        };

        let name = EnvironmentName::from_str(&name)?;

        let env =
            PathEnvironment::<Original>::init(&current_dir, name, flox.temp_dir.clone()).await?;

        println!(
            indoc::indoc! {"
            ✨ created environment {name} ({system})

            Enter the environment with \"flox activate\"
            Search and install packages with \"flox search {{packagename}}\" and \"flox install {{packagename}}\"
            "},
            name = env.environment_ref(),
            system = flox.system
        );
        Ok(())
    }
}

/// List (status?) packages installed in an environment
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct List {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(external(list_output), optional)]
    json: Option<ListOutput>,

    /// The generation to list, if not specified defaults to the current one
    #[bpaf(positional("GENERATION"))]
    generation: Option<u32>,
}

#[derive(Bpaf, Clone)]
pub enum ListOutput {
    /// Include store paths of packages in the environment
    #[bpaf(long("out-path"))]
    OutPath,
    /// Print as machine readable json
    #[bpaf(long)]
    Json,
}

impl List {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let env = resolve_environment(&flox, self.environment.as_deref(), "install").await?;

        let catalog = env
            .catalog(&flox.nix(Default::default()), &flox.system)
            .await
            .context("Could not get catalog")?;
        // let installed_store_paths = env.installed_store_paths(&flox).await?;

        println!("Packages in {}:", env.environment_ref());
        for (publish_element, _) in catalog.entries.iter() {
            if publish_element.version != LATEST_VERSION {
                println!(
                    "{} {}",
                    publish_element.to_flox_tuple(),
                    publish_element.version
                )
            } else {
                println!("{}", publish_element.to_flox_tuple())
            }
        }
        // for store_path in installed_store_paths.iter() {
        //     println!("{}", store_path.to_string_lossy())
        // }
        Ok(())
    }
}

/// list all available environments
/// Aliases:
///   environments, envs
#[derive(Bpaf, Clone)]
#[bpaf(command, long("environments"))]
pub struct Envs;
impl Envs {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let env = PathEnvironment::<Original>::discover(
            std::env::current_dir().unwrap(),
            flox.temp_dir.clone(),
        )?;

        if let Some(env) = env {
            println!("{}", env.environment_ref());
        } else {
            println!();
        }
        Ok(())
    }
}

/// Install a package into an environment
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Install {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(positional("PACKAGES"), some("At least one package"))]
    packages: Vec<String>,
}
impl Install {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let mut packages: Vec<_> = self
            .packages
            .iter()
            .map(|package| FloxPackage::parse(package, &flox.channels, DEFAULT_CHANNEL))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .dedup()
            .collect();

        let mut environment =
            resolve_environment(&flox, self.environment.as_deref(), "install").await?;

        // todo use set?
        // let installed = environment
        //     .packages(&flox.nix(Default::default()), &flox.system)
        //     .await?
        //     .into_iter()
        //     .map(From::from)
        //     .collect::<Vec<FloxPackage>>();

        // if let Some(installed) = packages.iter().find(|pkg| installed.contains(pkg)) {
        //     anyhow::bail!("{installed} is already installed");
        // }

        environment
            .install(
                packages.drain(..),
                &flox.nix(Default::default()),
                &flox.system,
            )
            .await
            .context("could not install packages")?;
        Ok(())
    }
}

/// Uninstall installed packages from an environment
#[derive(Bpaf, Clone)]
#[bpaf(command, long("remove"), long("rm"))]
pub struct Uninstall {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(positional("PACKAGES"), some("At least one package"))]
    packages: Vec<String>,
}
impl Uninstall {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let mut packages: Vec<_> = self
            .packages
            .iter()
            .map(|package| FloxPackage::parse(package, &flox.channels, DEFAULT_CHANNEL))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .dedup()
            .collect();

        let mut environment =
            resolve_environment(&flox, self.environment.as_deref(), "install").await?;

        environment
            .uninstall(
                packages.drain(..),
                &flox.nix(Default::default()),
                &flox.system,
            )
            .await
            .context("could not uninstall packages")?;
        Ok(())
    }
}

/// delete builds of non-current versions of an environment
#[derive(Bpaf, Clone)]
#[bpaf(command("wipe-history"))]
pub struct WipeHistory {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}

impl WipeHistory {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        let environment_name = self.environment.as_deref();
        let environment_ref: environment_ref::EnvironmentRef =
            resolve_environment_ref(&flox, "wipe-history", environment_name).await?;

        let env = PathEnvironment::<Original>::open(
            current_dir().unwrap(),
            environment_ref,
            flox.temp_dir.clone(),
        )
        .context("Environment not found")?;

        if env.delete_symlinks()? {
            // The flox nix instance is created with `--quiet --quiet`
            // because nix logs are passed to stderr unfiltered.
            // nix store gc logs are more useful,
            // thus we use 3 `--verbose` to have them appear.
            let nix = flox.nix::<NixCommandLine>(vec![
                "--verbose".to_string(),
                "--verbose".to_string(),
                "--verbose".to_string(),
            ]);
            let store_gc_command = StoreGc {
                ..StoreGc::default()
            };

            info!("Running garbage collection. This may take a while...");
            store_gc_command.run(&nix, &Default::default()).await?;
        } else {
            info!("No old generations found to clean up.")
        }
        Ok(())
    }
}

/// export declarative environment manifest to STDOUT
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Export {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}

impl Export {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

/// list environment generations with contents
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Generations {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long)]
    json: bool,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}
impl Generations {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}
/// access to the git CLI for floxmeta repository
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Git {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(any("Git Arguments", Some))]
    git_arguments: Vec<String>,
}
impl Git {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

/// show all versions of an environment
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct History {
    #[bpaf(long, short)]
    oneline: bool,

    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,
}
impl History {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

/// import declarative environment manifest from STDIN as new generation
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Import {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(external(ImportFile::parse), fallback(ImportFile::Stdin))]
    file: ImportFile,
}

impl Import {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

#[derive(Clone)]
pub enum ImportFile {
    Stdin,
    Path(PathBuf),
}

impl ImportFile {
    fn parse() -> impl Parser<ImportFile> {
        let stdin = bpaf::any("STDIN (-)", |t: char| {
            (t == '-').then_some(ImportFile::Stdin)
        })
        .help("Use `-` to read from STDIN")
        .complete(|_| vec![("-", Some("Read from STDIN"))]);
        let path = bpaf::positional("PATH")
            .help("Path to export file")
            .complete_shell(ShellComp::File { mask: None })
            .map(ImportFile::Path);
        construct!([stdin, path])
    }
}

/// Send environment to flox hub
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Push {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(external(push_floxmain_or_env), optional)]
    target: Option<PushFloxmainOrEnv>,

    /// forceably overwrite the remote copy of the environment
    #[bpaf(long, short)]
    force: bool,
}

impl Push {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

#[derive(Bpaf, Clone)]
pub enum PushFloxmainOrEnv {
    /// push the `floxmain` branch to sync configuration
    #[bpaf(long, short)]
    Main,
    Env {
        #[bpaf(long("environment"), short('e'), argument("ENV"))]
        env: Option<EnvironmentRef>,
    },
}

/// Pull environment from flox hub
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Pull {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(external(pull_floxmain_or_env), optional)]
    target: Option<PullFloxmainOrEnv>,

    /// forceably overwrite the local copy of the environment
    #[bpaf(long, short)]
    force: bool,
}
impl Pull {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

#[derive(Bpaf, Clone)]
pub enum PullFloxmainOrEnv {
    /// pull the `floxmain` branch to sync configuration
    #[bpaf(long, short)]
    Main,
    Env {
        #[bpaf(long("environment"), short('e'), argument("ENV"))]
        env: Option<EnvironmentRef>,
        /// do not actually render or create links to environments in the store.
        /// (Flox internal use only.)
        #[bpaf(long("no-render"))]
        no_render: bool,
    },
}

/// rollback to the previous generation of an environment
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Rollback {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    /// Generation to roll back to.
    ///
    /// If omitted, defaults to the previous generation.
    #[bpaf(argument("GENERATION"))]
    to: Option<u32>,
}
impl Rollback {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

/// switch to a specific generation of an environment
#[derive(Bpaf, Clone)]
#[bpaf(command("switch-generation"))]
pub struct SwitchGeneration {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(positional("GENERATION"))]
    generation: u32,
}

impl SwitchGeneration {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

/// upgrade packages using their most recent flake
#[derive(Bpaf, Clone)]
#[bpaf(command)]
pub struct Upgrade {
    #[allow(dead_code)] // pending spec for `-e`, `--dir` behaviour
    #[bpaf(external(environment_args), group_help("Environment Options"))]
    environment_args: EnvironmentArgs,

    #[bpaf(long, short, argument("ENV"))]
    environment: Option<EnvironmentRef>,

    #[bpaf(positional("PACKAGES"))]
    packages: Vec<String>,
}
impl Upgrade {
    pub async fn handle(self, flox: Flox) -> Result<()> {
        flox_forward(&flox).await
    }
}

async fn resolve_environment<'flox>(
    flox: &'flox Flox,
    environment_name: Option<&str>,
    subcommand: &str,
) -> Result<PathEnvironment<Original>, anyhow::Error> {
    let environment_ref = resolve_environment_ref(flox, subcommand, environment_name).await?;
    let environment = environment_ref
        .to_env(flox.temp_dir.clone())
        .context("Could not use environment")?;
    Ok(environment)
}

fn activate_run_args() -> impl Parser<Option<(String, Vec<String>)>> {
    let command = bpaf::positional("COMMAND").strict();
    let args = bpaf::any("ARGUMENTS", Some).many();

    bpaf::construct!(command, args).optional()
}
