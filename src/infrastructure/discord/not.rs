use crate::{
    domain::{
        Actor, Balance, GuildId as DomainGuildId, NotAmount, Price, PurchaseResult,
        UserId as DomainUserId,
    },
    infrastructure::{Error, MoneyError, ValidationError, types as values},
};
use serenity::all::*;

pub struct NotRepository<'a> {
    http: &'a Http,
}
impl<'a> NotRepository<'a> {
    pub fn new(http: &'a Http) -> Self {
        Self { http }
    }
    pub(super) fn parse_amount(value: &str) -> Result<NotAmount, Error> {
        Ok(values::amount(value.parse().map_err(
            |_| "増減額は-9223372036854775808〜9223372036854775807の整数で指定してください。",
        )?))
    }
    pub(super) fn parse_guild_id(value: &str) -> Result<DomainGuildId, ValidationError> {
        values::parse_guild_id(value)
    }
    pub(super) fn guild_id(guild: GuildId) -> Result<DomainGuildId, ValidationError> {
        values::guild_id(guild.get())
    }
    pub(super) fn balance_after_purchase(
        balance: Balance,
        price: Price,
    ) -> Result<Balance, MoneyError> {
        values::apply_balance(balance, values::amount(-price.get()))
    }
    pub(super) fn purchase_actor(purchase: &PurchaseResult) -> Actor {
        values::actor(
            purchase.item().guild_id(),
            purchase.buyer().user_id(),
            false,
        )
    }
    pub(super) async fn actor(
        &self,
        current: Option<GuildId>,
        supplied: Option<&str>,
        user: UserId,
    ) -> Result<Actor, Error> {
        let guild = Self::target_guild(current, supplied)?;
        self.authenticate(guild, user).await
    }
    pub(super) async fn authenticate(
        &self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Actor, Error> {
        // キャッシュや入力フラグで認可しない。RESTでBotの参加と本人の所属・ロールを確認する。
        let guild = self.http.get_guild(guild_id).await?;
        let member = self.http.get_member(guild_id, user_id).await?;
        Ok(Self::authenticated_actor(&guild, &member, user_id)?)
    }
    pub(super) fn authenticated_actor(
        guild: &PartialGuild,
        member: &Member,
        expected_user: UserId,
    ) -> Result<Actor, &'static str> {
        if member.user.id != expected_user || member.guild_id != guild.id {
            return Err("対象サーバーへの所属を確認できませんでした。");
        }
        let can_manage = guild.member_permissions(member).administrator();
        Ok(values::actor(
            Self::guild_id(guild.id).map_err(|_| "サーバーIDが不正です。")?,
            values::user_id(expected_user.get()).map_err(|_| "ユーザーIDが不正です。")?,
            can_manage,
        ))
    }
    fn target_guild(
        current: Option<GuildId>,
        supplied: Option<&str>,
    ) -> Result<GuildId, &'static str> {
        let supplied = supplied
            .map(|value| {
                Self::parse_guild_id(value)
                    .map(|id| GuildId::new(id.get()))
                    .map_err(|_| "サーバーIDが不正です。")
            })
            .transpose()?;
        match (current, supplied) {
            (Some(current), Some(supplied)) if current != supplied => {
                Err("サーバー内ではそのサーバーを指定してください。")
            }
            (Some(current), _) => Ok(current),
            (None, Some(supplied)) => Ok(supplied),
            (None, None) => Err("DMでは guild に対象サーバーを指定してください。"),
        }
    }
    pub(super) async fn adjustment_target(
        &self,
        actor: Actor,
        user: UserId,
    ) -> Result<DomainUserId, Error> {
        if !actor.can_manage_not() {
            return Err("この操作には対象サーバーの管理者権限が必要です。".into());
        }
        self.member_target(actor, user).await
    }
    pub(super) async fn member_target(
        &self,
        actor: Actor,
        user: UserId,
    ) -> Result<DomainUserId, Error> {
        let member = self
            .http
            .get_member(GuildId::new(actor.guild_id().get()), user)
            .await
            .map_err(|_| "対象ユーザーの所属を確認できませんでした。")?;
        if member.user.id != user || member.guild_id.get() != actor.guild_id().get() {
            return Err("対象ユーザーの所属を確認できませんでした。".into());
        }
        Ok(values::user_id(user.get())?)
    }
    pub(super) async fn participant(
        &self,
        guild: GuildId,
        user: UserId,
    ) -> Result<Option<Actor>, Error> {
        let member = match self.http.get_member(guild, user).await {
            Ok(member) => member,
            Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
                response,
            ))) if response.error.code == 10007 => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if member.user.id != user || member.guild_id != guild || member.user.bot {
            return Ok(None);
        }
        Ok(Some(values::actor(
            Self::guild_id(guild)?,
            values::user_id(user.get())?,
            false,
        )))
    }
}

#[cfg(test)]
#[path = "../../../tests/discord/not.rs"]
mod not_tests;
