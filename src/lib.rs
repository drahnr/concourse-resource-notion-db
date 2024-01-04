use concourse_resource::{InOutput, OutOutput, Resource};

use notion::{
    ids::{AsIdentifier, BlockId, DatabaseId},
    models::{
        search::{DatabaseQuery, FilterCondition, PropertyCondition, SelectCondition},
        Database, DateTime, Page, PageCreateRequest, Parent, Properties, Utc,
    },
    NotionApi,
};
use reqwest::{Method, Url};

use color_eyre::eyre::*;
use serde::{Deserialize, Serialize};
use std::future::Future;

// use
use fs_err as fs;

#[derive(Deserialize, Serialize, PartialEq, Eq, Clone)]
pub struct Version {
    id: DatabaseId,
    last_edited_time: DateTime<Utc>,
}

#[derive(Deserialize, Serialize)]
pub struct SourceConfig {
    /// The API token for accessing the notion database.
    api_token: String,
    /// The database name to modify.
    database: String,
}

async fn notion_api_client(config: &SourceConfig) -> notion::NotionApi {
    notion::NotionApi::new(config.api_token.clone()).expect("Creating notion API client works. qed")
}

use std::str::FromStr;

async fn lookup_db(notion: &NotionApi, config: &SourceConfig) -> Result<Database> {
    let db_id =
        DatabaseId::from_str(&config.database).wrap_err("Failed to convert database to ID")?;
    let db = notion.get_database(db_id).await?;
    Ok(db)
}

async fn check(config: SourceConfig) -> Result<Version> {
    let notion = notion_api_client(&config).await;
    let db = lookup_db(&notion, &config).await?;
    Ok(Version {
        id: db.id,
        last_edited_time: db.last_edited_time,
    })
}

async fn get(config: SourceConfig, at: Version) -> Result<Vec<Page>> {
    let notion = notion_api_client(&config).await;
    let db = lookup_db(&notion, &config).await?;
    let reality = Version {
        id: db.id.clone(),
        last_edited_time: db.last_edited_time,
    };
    if at != reality {
        bail!("Database was modified since checking")
    }
    let items = notion
        .query_database(db.id, DatabaseQuery::default())
        .await?;
    Ok(items.results)
}

#[allow(dead_code)]
async fn wild<B, R>(api_token: &str, method: Method, path: &str, body: B) -> Result<R>
where
    B: Serialize,
    R: for<'de> Deserialize<'de>,
{
    let client = reqwest::Client::new();
    let req = client
        .request(
            method,
            Url::parse(&("https://api.notion.com".to_owned() + path)).unwrap(),
        )
        .bearer_auth(api_token)
        .header("Notion-Version", "2022-06-28")
        .json(&body)
        .build()?;

    let resp = client.execute(req).await?;
    if resp.status().as_u16() != 200 {
        bail!(
            "Failed submission with response code {}",
            resp.status().as_u16()
        );
    } else {
        let resp: R = resp.json().await?;
        Ok(resp)
    }
}

#[allow(dead_code)]
async fn wild_no_payload(api_token: &str, method: Method, path: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let req = client
        .request(
            method,
            Url::parse(&("https://api.notion.com".to_owned() + path)).unwrap(),
        )
        .bearer_auth(api_token)
        .header("Notion-Version", "2022-06-28")
        .build()?;

    let resp = client.execute(req).await?;
    if resp.status().as_u16() != 200 {
        bail!(
            "Failed submission with response code {}",
            resp.status().as_u16()
        );
    } else {
        Ok(())
    }
}

async fn page_delete(api_key: &str, page: &Page) -> Result<()> {
    let block_id = BlockId::from(page.id.clone());
    eprintln!("Attempting to delete page {block_id}");
    wild_no_payload(api_key, Method::DELETE, &format!("/v1/blocks/{block_id}")).await?;
    Ok(())
}

fn db_version(db: &Database) -> Version {
    let version = Version {
        id: db.id.clone(),
        last_edited_time: db.last_edited_time,
    };
    version
}

async fn put(config: SourceConfig, updates: Vec<Properties>, mode: Mode) -> Result<Version> {
    let notion = notion_api_client(&config).await;
    let db = lookup_db(&notion, &config).await?;

    if updates.is_empty() {
        bail!("Nothing to update");
    }
    // let mut version = db_version(&db);
    if let Mode::Replace = &mode {
        let pages = notion
            .query_database(db.as_id(), DatabaseQuery::default())
            .await?;
        for page in pages.results() {
            page_delete(&config.api_token, page).await?;
        }
    }
    let n = updates.len();
    for (idx, update) in updates.into_iter().enumerate() {
        eprintln!("Applying update: {idx}/{n}", idx = idx + 1);
        match &mode {
            Mode::Append | Mode::Replace => {}
            Mode::Update {
                ref primary_id_property,
            } => {
                let value = update.properties.get(primary_id_property).ok_or_else(|| {
                    eyre!(
                        "No such column, must be one of {:?}",
                        update
                            .properties
                            .keys()
                            .collect::<std::collections::HashSet<_>>()
                    )
                })?;
                let value = serde_json::to_string(value)?;
                let property = primary_id_property.clone();
                let pages = notion
                    .query_database(
                        db.as_id(),
                        DatabaseQuery {
                            filter: Some(FilterCondition::Property {
                                property,
                                condition: PropertyCondition::Select(SelectCondition::Equals(
                                    value,
                                )),
                            }),
                            ..Default::default()
                        },
                    )
                    .await?;
                for page in pages.results() {
                    page_delete(&config.api_token, page).await?;
                }
            }
        }
        let page_create_req = PageCreateRequest {
            parent: Parent::Database {
                database_id: db.id.clone(),
            },
            properties: update,
            children: None,
        };
        let _page = notion.create_page(page_create_req).await?;
        // version.last_edited_time = page.last_edited_time;
    }

    // fetch the version
    let db = lookup_db(&notion, &config).await?;
    let version = db_version(&db);
    Ok(version)
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Append,
    Replace,
    Update {
        primary_id_property: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutParams {
    /// The path that contains a json array of rows: `[{ {..}, {..}, {..},.. }]`.
    #[serde(default)]
    pub path: std::path::PathBuf,
    /// The mode in which data is being modified.
    #[serde(default)]
    pub mode: Mode,
}

impl Default for OutParams {
    fn default() -> Self {
        Self {
            path: std::path::PathBuf::from("out.json"),
            mode: Mode::default(),
        }
    }
}

fn run_this<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    let rt = tokio::runtime::Runtime::new()?;
    let output = rt.block_on(future)?;
    Ok(output)
}

#[derive(Debug, Clone)]
pub struct NotionResource;

impl Resource for NotionResource {
    type Version = Version;
    type Source = SourceConfig;
    type InParams = concourse_resource::Empty;
    type InMetadata = concourse_resource::Empty;
    type OutParams = OutParams;
    type OutMetadata = concourse_resource::Empty;

    fn resource_check(
        source: Option<Self::Source>,
        _version: Option<Self::Version>,
    ) -> Vec<Self::Version> {
        let source = source.expect("Must provide `source:` values in resource configuration");
        let version = run_this(check(source)).expect("Version query should succeed");
        vec![version]
    }

    fn resource_in(
        source: Option<Self::Source>,
        version: Self::Version,
        _params: Option<Self::InParams>,
        output_path: &str,
    ) -> Result<InOutput<Self::Version, Self::InMetadata>, Box<dyn std::error::Error>> {
        let source = source.expect("Must provide `source:` values in resource configuration");
        let items = run_this(get(source, version.clone()))?;
        let s = serde_json::to_string(&items)?;
        fs::write(output_path, s.as_bytes())?;
        std::result::Result::Ok::<_, Box<dyn std::error::Error + 'static>>(InOutput {
            version,
            metadata: None,
        })
    }

    fn resource_out(
        source: Option<Self::Source>,
        params: Option<Self::OutParams>,
        input_path: &str,
    ) -> OutOutput<Self::Version, Self::OutMetadata> {
        let source = source.expect("Must provide `source:` values in resource configuration");
        let OutParams { path, mode } = params.unwrap_or_default();
        let input_path = std::path::PathBuf::from(input_path);
        let consume = input_path.join(path);
        let consume = if !input_path.is_absolute() {
            std::env::current_dir().unwrap().join(consume)
        } else {
            consume
        };
        eprintln!("Loading data from {} for out step.", consume.display());
        let items = fs::OpenOptions::new()
            .read(true)
            .open(consume)
            .expect("Huh? File? You there?");
        let items: Vec<Properties> = serde_json::from_reader(items)
            .expect("User provided get/put properties invalid/unknown");
        let new_version = run_this(put(source, items, mode)).expect("Shall never fail. qed");
        OutOutput {
            version: new_version,
            metadata: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_params_update() -> Result<(), ErrReport> {
        let y = OutParams {
            mode: Mode::Update {
                primary_id_property: "foo".to_owned(),
            },
            path: std::path::PathBuf::from("tmp/foo.json"),
        };
        let json_str = serde_json::to_string(&y)?;
        eprintln!("{json_str}");
        let x: OutParams = serde_json::from_str(&json_str)?;
        assert_eq!(x, y);
        Ok(())
    }

    #[test]
    fn out_params_plain() -> Result<(), ErrReport> {
        let y = OutParams {
            mode: Mode::Replace,
            path: std::path::PathBuf::from("tmp/foo.json"),
        };
        let json_str = serde_json::to_string(&y)?;
        eprintln!("{json_str}");
        let x: OutParams = serde_json::from_str(&json_str)?;
        assert_eq!(x, y);
        Ok(())
    }

    #[test]
    fn optional_mode() {
        let _: OutParams = serde_json::from_str(r###"{"path":"tmp/foo.json"}"###).unwrap();
    }
    #[test]
    fn optional_path() {
        let _: OutParams = serde_json::from_str(r###"{"mode":"Replace"}"###).unwrap();
    }
}
