use color_eyre::eyre::{bail, Result};
use concourse_resource::*;
use concourse_resource_notion_db::NotionResource;

fn main() -> Result<()> {
    color_eyre::install()?;
    run::<NotionResource>()?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubCommand {
    In,
    Out,
    Check,
}

struct Args {
    subcommand: SubCommand,
    path: Option<String>,
}

impl TryFrom<std::env::Args> for Args {
    type Error = color_eyre::eyre::Report;
    fn try_from(mut args: std::env::Args) -> Result<Self> {
        let subcommand = match args.next().as_ref().map(AsRef::as_ref) {
            Some("/opt/resource/check") => SubCommand::Check,
            Some("/opt/resource/in") => SubCommand::In,
            Some("/opt/resource/out") => SubCommand::Out,
            _ => match args.next().as_ref().map(AsRef::as_ref) {
                Some("check") => SubCommand::Check,
                Some("in") => SubCommand::In,
                Some("out") => SubCommand::Out,
                Some(x) => bail!("Unexpected subcommand: {x}"),
                None => bail!(
                    "Not in a resource (/opt/resource/{{in,out,check}}) nor using a subcommand"
                ),
            },
        };
        Ok(Self {
            subcommand,
            path: args.next(),
        })
    }
}

impl Args {
    pub fn subcommand(&self) -> SubCommand {
        self.subcommand
    }
    pub fn path(&self) -> Option<&str> {
        self.path.as_ref().map(AsRef::as_ref)
    }
}

fn run<R: Resource>() -> Result<()> {
    use std::io::Read;

    use concourse_resource::internal::*;
    let args = Args::try_from(std::env::args())?;

    let mut input_buffer = String::new();
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();

    handle.read_to_string(&mut input_buffer)?;

    match args.subcommand() {
        SubCommand::Check => {
            let input: CheckInput<<R as Resource>::Source, <R as Resource>::Version> =
                serde_json::from_str(&input_buffer)?;
            let result = <R as Resource>::resource_check(input.source, input.version);
            println!(
                "{}",
                serde_json::to_string(&result).expect("error serializing output")
            );
        }
        SubCommand::In => {
            let input: InInput<
                <R as Resource>::Source,
                <R as Resource>::Version,
                <R as Resource>::InParams,
            > = serde_json::from_str(&input_buffer)?;
            let result = <R as Resource>::resource_in(
                input.source,
                input.version,
                input.params,
                args.path().expect("expected path as first parameter"),
            );
            match result {
                Err(error) => {
                    eprintln!("Error! {}", error);
                    std::process::exit(1);
                }
                Ok(InOutput { version, metadata }) => println!(
                    "{}",
                    serde_json::to_string(&InOutputKV {
                        version,
                        metadata: metadata.map(|md| md.into_metadata_kv())
                    })
                    .expect("error serializing output")
                ),
            };
        }
        SubCommand::Out => {
            let input: OutInput<<R as Resource>::Source, <R as Resource>::OutParams> =
                serde_json::from_str(&input_buffer).expect("error deserializing input");
            let result = <R as Resource>::resource_out(
                input.source,
                input.params,
                args.path().expect("expected path as first parameter"),
            );
            println!(
                "{}",
                serde_json::to_string(&OutOutputKV {
                    version: result.version,
                    metadata: result.metadata.map(|md| md.into_metadata_kv())
                })
                .expect("error serializing output")
            );
        }
    }
    Ok(())
}
