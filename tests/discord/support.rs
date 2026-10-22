use super::Data;
use crate::domain;
use crate::{
    domain::{Actor, ShopItem},
    infrastructure::{MAX_CONTENT_BYTES, sqlite::SqliteDatabaseAccess},
};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serenity::{
    all::{ComponentInteraction, Http},
    http::HttpBuilder,
    json::{Value, from_value, json},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

pub(super) struct TestDirectory(pub(super) PathBuf);
impl TestDirectory {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "notbot-p08-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn actor() -> Actor {
    domain::Actor::new(
        domain::GuildId::new(100).unwrap(),
        domain::UserId::new(200).unwrap(),
        false,
    )
}

pub(super) fn message() -> Value {
    json!({"id":"900","channel_id":"400","author":{"id":"42","username":"bot","discriminator":"0","avatar":null,"bot":true},"content":"","timestamp":"2026-10-19T00:00:00Z","edited_timestamp":null,"tts":false,"mention_everyone":false,"mentions":[],"mention_roles":[],"attachments":[],"embeds":[],"pinned":false,"type":0,"components":[]})
}

pub(super) fn component(custom_id: &str, user: u64) -> ComponentInteraction {
    from_value(json!({"id":"910","application_id":"42","data":{"component_type":2,"custom_id":custom_id},
        "channel_id":"700","user":{"id":user.to_string(),"username":"buyer","discriminator":"0","avatar":null},
        "token":"offline-interaction-token","version":1,"message":message(),"locale":"ja","entitlements":[],
        "attachment_size_limit":MAX_CONTENT_BYTES})).unwrap()
}

pub(super) struct MockDiscord {
    pub(super) address: String,
    pub(super) requests: Arc<StdMutex<Vec<(String, String)>>>,
    pub(super) fail_post: Arc<AtomicBool>,
    pub(super) wrong_channel: Arc<AtomicBool>,
    pub(super) deny_embed: Arc<AtomicBool>,
    pub(super) deny_dm: Arc<AtomicBool>,
    pub(super) fail_delivery: Arc<AtomicBool>,
    pub(super) deny_member: Arc<AtomicBool>,
    pub(super) missing_message: Arc<AtomicBool>,
    pub(super) reward_message: Arc<StdMutex<Value>>,
    pub(super) role_definitions: Arc<StdMutex<Vec<Value>>>,
    pub(super) role_members: Arc<StdMutex<HashSet<(u64, u64)>>>,
    pub(super) role_change_mode: Arc<AtomicU64>,
    pub(super) fail_role_lookup: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockDiscord {
    pub(super) fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let fail_post = Arc::new(AtomicBool::new(false));
        let wrong_channel = Arc::new(AtomicBool::new(false));
        let deny_embed = Arc::new(AtomicBool::new(false));
        let deny_dm = Arc::new(AtomicBool::new(false));
        let fail_delivery = Arc::new(AtomicBool::new(false));
        let deny_member = Arc::new(AtomicBool::new(false));
        let missing_message = Arc::new(AtomicBool::new(false));
        let mut original = message();
        original["author"]["id"] = json!("200");
        original["author"]["bot"] = json!(false);
        original["guild_id"] = json!("100");
        let reward_message = Arc::new(StdMutex::new(original));
        let original = reward_message.clone();
        let role_definitions = Arc::new(StdMutex::new(vec![
            json!({"id":"500","name":"会員","color":0,"colors":{"primary_color":0},"hoist":false,"position":1,"permissions":"0","managed":false,"mentionable":false}),
            json!({"id":"600","name":"Bot","color":0,"colors":{"primary_color":0},"hoist":false,"position":2,"permissions":"268435456","managed":true,"mentionable":false}),
        ]));
        let role_members = Arc::new(StdMutex::new(HashSet::new()));
        let role_change_mode = Arc::new(AtomicU64::new(0));
        let fail_role_lookup = Arc::new(AtomicBool::new(false));
        let (definitions, role_holders, role_mode, lookup_failed) = (
            role_definitions.clone(),
            role_members.clone(),
            role_change_mode.clone(),
            fail_role_lookup.clone(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let (r, fail, wrong, deny, done) = (
            requests.clone(),
            fail_post.clone(),
            wrong_channel.clone(),
            deny_embed.clone(),
            stop.clone(),
        );
        let (block_dm, fail_file, block_member) =
            (deny_dm.clone(), fail_delivery.clone(), deny_member.clone());
        let missing = missing_message.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        break Some(end + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let route = headers.lines().next().unwrap().to_owned();
                let body =
                    String::from_utf8(bytes[header_end..header_end + length].to_vec()).unwrap();
                let is_file = body.contains("name=\"files[0]\"");
                r.lock().unwrap().push((route.clone(), body));
                let (status, value) = if route.contains("/channels/400/messages")
                    && (route.starts_with("POST ") || route.starts_with("PATCH "))
                {
                    if route.starts_with("PATCH ") && missing.swap(false, Ordering::Relaxed) {
                        (404, json!({"code":10008,"message":"Unknown Message"}))
                    } else if fail.load(Ordering::Relaxed) {
                        (
                            403,
                            json!({"code":50013,"message":"private-external-error"}),
                        )
                    } else {
                        let mut response = message();
                        if route.starts_with("PATCH ") {
                            response["id"] = json!(
                                route
                                    .split("/messages/")
                                    .nth(1)
                                    .unwrap()
                                    .split_whitespace()
                                    .next()
                                    .unwrap()
                            );
                        }
                        (200, response)
                    }
                } else if route.starts_with("GET ") && route.contains("/channels/400/messages/900 ")
                {
                    (200, original.lock().unwrap().clone())
                } else if route.contains("/interactions/") && route.contains("/callback") {
                    (204, Value::Null)
                } else if route.starts_with("PATCH ") && route.contains("/webhooks/") {
                    (200, message())
                } else if route.contains("/users/@me/channels") {
                    (
                        200,
                        json!({"id":"700","type":1,"recipients":[{"id":"201","username":"buyer","discriminator":"0","avatar":null}]}),
                    )
                } else if route.starts_with("POST ") && route.contains("/channels/700/messages") {
                    if block_dm.load(Ordering::Relaxed)
                        || (is_file && fail_file.load(Ordering::Relaxed))
                    {
                        (
                            403,
                            json!({"code":50007,"message":"private-external-error"}),
                        )
                    } else {
                        let mut response = message();
                        response["channel_id"] = json!("700");
                        (200, response)
                    }
                } else if route.contains("/channels/400 ") {
                    (
                        200,
                        json!({"id":"400","guild_id":if wrong.load(Ordering::Relaxed){"101"}else{"100"},"type":0,"name":"shop","position":0,"permission_overwrites":[],"nsfw":false,"parent_id":null}),
                    )
                } else if route.contains("/users/@me ") {
                    (
                        200,
                        json!({"id":"42","username":"bot","discriminator":"0","avatar":null,"bot":true,"verified":true,"mfa_enabled":false}),
                    )
                } else if (route.starts_with("PUT ") || route.starts_with("DELETE "))
                    && route.contains("/roles/500 ")
                {
                    let guild: u64 = route
                        .split("/guilds/")
                        .nth(1)
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let user: u64 = route
                        .split("/members/")
                        .nth(1)
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let mode = role_mode.load(Ordering::Relaxed);
                    if mode == 0 || mode == 2 {
                        if route.starts_with("PUT ") {
                            role_holders.lock().unwrap().insert((guild, user));
                        } else {
                            role_holders.lock().unwrap().remove(&(guild, user));
                        }
                    }
                    if mode == 3 {
                        lookup_failed.store(true, Ordering::Relaxed);
                    }
                    if mode == 0 {
                        (204, Value::Null)
                    } else {
                        (403, json!({"code":50013,"message":"private-role-error"}))
                    }
                } else if route.contains("/members/") {
                    let id = route
                        .split("/members/")
                        .nth(1)
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap();
                    let guild: u64 = route
                        .split("/guilds/")
                        .nth(1)
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let mut roles = if id == "42" { vec!["600"] } else { vec![] };
                    if role_holders
                        .lock()
                        .unwrap()
                        .contains(&(guild, id.parse().unwrap()))
                    {
                        roles.push("500");
                    }
                    if lookup_failed.load(Ordering::Relaxed) && id != "42" {
                        (
                            403,
                            json!({"code":50013,"message":"private-role-lookup-error"}),
                        )
                    } else if block_member.load(Ordering::Relaxed) && id != "42" {
                        (404, json!({"code":10007,"message":"Unknown Member"}))
                    } else {
                        (
                            200,
                            json!({"user":{"id":id,"username":"member","discriminator":"0","avatar":null,"bot":id=="42"},"guild_id":guild.to_string(),"nick":null,"roles":roles,"joined_at":"2026-10-19T00:00:00Z","deaf":false,"mute":false,"flags":0}),
                        )
                    }
                } else if route.contains("/guilds/100 ") || route.contains("/guilds/101 ") {
                    let guild = if route.contains("/guilds/101 ") {
                        "101"
                    } else {
                        "100"
                    };
                    let permissions = if deny.load(Ordering::Relaxed) {
                        1024 | 2048
                    } else {
                        1024 | 2048 | 16384
                    };
                    let mut response = json!({"id":guild,"name":"テストサーバー","icon":null,"owner_id":"999","afk_timeout":300,"verification_level":0,"default_message_notifications":0,"explicit_content_filter":0,"roles":[{"id":guild,"name":"@everyone","color":0,"colors":{"primary_color":0},"hoist":false,"position":0,"permissions":permissions.to_string(),"managed":false,"mentionable":false}],"emojis":[],"features":[],"mfa_level":0,"system_channel_flags":0,"premium_tier":0,"nsfw_level":0,"preferred_locale":"ja","stickers":[],"premium_progress_bar_enabled":false});
                    response["roles"]
                        .as_array_mut()
                        .unwrap()
                        .extend(definitions.lock().unwrap().clone());
                    (200, response)
                } else {
                    (404, json!({"code":10003,"message":"unknown-route"}))
                };
                let body = if status == 204 {
                    String::new()
                } else {
                    value.to_string()
                };
                write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            address,
            requests,
            fail_post,
            wrong_channel,
            deny_embed,
            deny_dm,
            fail_delivery,
            deny_member,
            missing_message,
            reward_message,
            role_definitions,
            role_members,
            role_change_mode,
            fail_role_lookup,
            stop,
            worker: Some(worker),
        }
    }
    pub(super) fn http(&self) -> Http {
        HttpBuilder::new("offline-test-token")
            .application_id(serenity::all::ApplicationId::new(42))
            .proxy(&self.address)
            .ratelimiter_disabled(true)
            .build()
    }
    pub(super) fn posts(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(route, _)| route.starts_with("POST "))
            .map(|(_, body)| body.clone())
            .collect()
    }
}
impl Drop for MockDiscord {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

pub(super) async fn setup(directory: &TestDirectory) -> Data {
    let database = SqliteDatabaseAccess::open(directory.0.join("db.sqlite"))
        .await
        .unwrap();
    Data::new(
        database,
        directory.0.join("private"),
        HashMap::from([(actor().guild_id(), domain::ChannelId::new(400).unwrap())]),
    )
    .unwrap()
}

pub(super) async fn saved_item(data: &Data) -> ShopItem {
    let saved = data
        .discord_shop
        .store_content(actor(), "private-save-key", "資料.pdf", &b"pdf"[..])
        .unwrap();
    data.sqlite_shop
        .create(
            actor(),
            domain::ItemDetails::new("資料集", "商品説明", domain::Price::new(500).unwrap())
                .unwrap(),
            saved,
        )
        .await
        .unwrap()
}

pub(super) async fn raw(directory: &TestDirectory) -> DatabaseConnection {
    let path = directory.0.join("db.sqlite");
    let mut options = ConnectOptions::new("sqlite://unused");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| options.filename(&path).foreign_keys(true));
    Database::connect(options).await.unwrap()
}
