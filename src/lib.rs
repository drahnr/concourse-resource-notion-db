use concourse_resource::{InOutput, OutOutput, Resource};

use notion::{
    ids::DatabaseId,
    models::{
        search::{DatabaseQuery, FilterProperty, FilterValue, NotionSearch},
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
    return Ok(db);

    let objects = notion
        .search(NotionSearch::Query(config.database.to_string()))
        .await
        .wrap_err("Failed to search for msg")?;
    let mut dbs = Vec::from_iter(
        objects
            .only_databases()
            .results
            .into_iter()
            .filter(|x| x.title_plain_text() == config.database),
    );
    let db = match dbs.len() {
        0 => bail!("Counldn't find that DB: {}", config.database),
        1 => dbs
            .pop()
            .expect("We have one item, hence pop() always works. qed"),
        n => bail!("Ambiguous db name, found {n} matches."),
    };
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
async fn wild<B, R>(api_token: String, _method: Method, path: &str, body: B) -> Result<R>
where
    B: Serialize,
    R: for<'de> Deserialize<'de>,
{
    let client = reqwest::Client::new();
    let req = client
        .request(
            Method::POST,
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

async fn put(config: SourceConfig, inject: Vec<Properties>) -> Result<Version> {
    let notion = notion_api_client(&config).await;
    let db = lookup_db(&notion, &config).await?;

    if inject.is_empty() {
        bail!("Nothing to update");
    }
    let mut version = Version {
        id: db.id.clone(),
        last_edited_time: db.last_edited_time,
    };
    for extra in inject {
        let page_create_req = PageCreateRequest {
            parent: Parent::Database {
                database_id: db.id.clone(),
            },
            properties: extra,
            children: None,
        };
        let page = notion.create_page(page_create_req).await?;
        version.last_edited_time = page.last_edited_time;
    }

    Ok(version)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutParams {
    /// The path that contains a json array of rows: `[{ {..}, {..}, {..},.. }]`.
    pub path: std::path::PathBuf,
}

impl Default for OutParams {
    fn default() -> Self {
        Self {
            path: std::path::PathBuf::from("out.json"),
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
        let params = params.unwrap_or_default();
        let input_path = std::path::PathBuf::from(input_path);
        let consume = input_path.join(params.path);
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
        let new_version = run_this(put(source, items)).expect("Shall never fail. qed");
        OutOutput {
            version: new_version,
            metadata: None,
        }
    }
}
