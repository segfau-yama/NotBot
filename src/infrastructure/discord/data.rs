use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
};

use tokio::sync::Mutex;

use super::{listings, posting};
use crate::{
    domain::{ChannelId, GuildId, ItemId, PurchaseQuote},
    infrastructure::{
        Error,
        sqlite::{SqliteDatabaseAccess, not::NotUseCase, shop::ShopUseCase},
    },
};

pub struct Data {
    pub(super) not: NotUseCase,
    pub(super) shop: ShopUseCase,
    pub(super) database: SqliteDatabaseAccess,
    pub(super) content_dir: PathBuf,
    pub(super) shop_channels: HashMap<GuildId, ChannelId>,
    pub(super) download: reqwest::Client,
    pub(super) publication_locks: Mutex<HashMap<ItemId, Weak<Mutex<()>>>>,
    pub(super) drafts: Mutex<HashMap<u64, listings::ListingDraft>>,
    pub(super) uploads: Mutex<HashMap<u64, posting::Upload>>,
    pub(super) confirmations: Mutex<HashMap<String, PurchaseQuote>>,
}

impl Data {
    pub(super) async fn publication_lock(&self, item: ItemId) -> Arc<Mutex<()>> {
        let mut locks = self.publication_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&item).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(item, Arc::downgrade(&lock));
        lock
    }
}

pub type Context<'a> = poise::ApplicationContext<'a, Data, Error>;
