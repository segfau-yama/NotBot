use crate::domain;
use crate::{
    domain::{Actor, GuildId as DomainGuildId, NotAmount, UserId as DomainUserId},
    infrastructure::{Error, ValidationError, types as values},
};
use serenity::all::*;

pub struct NotRepository;

impl NotRepository {
    pub(super) fn parse_amount(&self, value: &str) -> Result<NotAmount, Error> {
        Ok(domain::NotAmount::new(value.parse().map_err(
            |_| "増減額は-9223372036854775808〜9223372036854775807の整数で指定してください。",
        )?))
    }
    pub(super) fn parse_guild_id(&self, value: &str) -> Result<DomainGuildId, ValidationError> {
        values::parse_guild_id(value)
    }
    pub(super) fn guild_id(&self, guild: GuildId) -> Result<DomainGuildId, ValidationError> {
        domain::GuildId::new(guild.get())
    }
    pub(super) async fn actor(
        &self,
        http: &Http,
        current: Option<GuildId>,
        supplied: Option<&str>,
        user: UserId,
    ) -> Result<Actor, Error> {
        let guild = self.target_guild(current, supplied)?;
        self.authenticate(http, guild, user).await
    }
    pub(super) async fn authenticate(
        &self,
        http: &Http,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Actor, Error> {
        // キャッシュや入力フラグで認可しない。RESTでBotの参加と本人の所属・ロールを確認する。
        let guild = http.get_guild(guild_id).await?;
        let member = http.get_member(guild_id, user_id).await?;
        Ok(self.authenticated_actor(&guild, &member, user_id)?)
    }
    pub(super) fn authenticated_actor(
        &self,
        guild: &PartialGuild,
        member: &Member,
        expected_user: UserId,
    ) -> Result<Actor, &'static str> {
        if member.user.id != expected_user || member.guild_id != guild.id {
            return Err("対象サーバーへの所属を確認できませんでした。");
        }
        let can_manage = guild.member_permissions(member).administrator();
        Ok(domain::Actor::new(
            self.guild_id(guild.id)
                .map_err(|_| "サーバーIDが不正です。")?,
            domain::UserId::new(expected_user.get()).map_err(|_| "ユーザーIDが不正です。")?,
            can_manage,
        ))
    }
    fn target_guild(
        &self,
        current: Option<GuildId>,
        supplied: Option<&str>,
    ) -> Result<GuildId, &'static str> {
        let supplied = supplied
            .map(|value| {
                self.parse_guild_id(value)
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
        http: &Http,
        actor: Actor,
        user: UserId,
    ) -> Result<DomainUserId, Error> {
        if !actor.can_manage_not() {
            return Err("この操作には対象サーバーの管理者権限が必要です。".into());
        }
        self.member_target(http, actor, user).await
    }
    pub(super) async fn member_target(
        &self,
        http: &Http,
        actor: Actor,
        user: UserId,
    ) -> Result<DomainUserId, Error> {
        let member = http
            .get_member(GuildId::new(actor.guild_id().get()), user)
            .await
            .map_err(|_| "対象ユーザーの所属を確認できませんでした。")?;
        if member.user.id != user || member.guild_id.get() != actor.guild_id().get() {
            return Err("対象ユーザーの所属を確認できませんでした。".into());
        }
        Ok(domain::UserId::new(user.get())?)
    }
    pub(super) async fn participant(
        &self,
        http: &Http,
        guild: GuildId,
        user: UserId,
    ) -> Result<Option<Actor>, Error> {
        let member = match http.get_member(guild, user).await {
            Ok(member) => member,
            Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
                response,
            ))) if response.error.code == 10007 => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if member.user.id != user || member.guild_id != guild || member.user.bot {
            return Ok(None);
        }
        Ok(Some(domain::Actor::new(
            self.guild_id(guild)?,
            domain::UserId::new(user.get())?,
            false,
        )))
    }
}

#[cfg(test)]
#[path = "../../../tests/discord/not.rs"]
mod not_tests;
