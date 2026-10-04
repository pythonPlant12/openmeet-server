//! Online presence and who may see it. Every endpoint that reports `isOnline` goes through this
//! module, so a per-user privacy setting only needs to change `visibility_for`.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use crate::schema::{friendships, user_presence};

/// Clients heartbeat presence more often than this, so a newer timestamp means the user is online.
const ONLINE_WINDOW_SECONDS: i64 = 45;

/// Who can see a user's online status.
#[allow(dead_code)] // Everyone, Friends, and Nobody become reachable once users pick a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceVisibility {
    Everyone,
    /// Accepted friends and people who share a group with the user.
    Contacts,
    Friends,
    Nobody,
}

pub const DEFAULT_PRESENCE_VISIBILITY: PresenceVisibility = PresenceVisibility::Contacts;

/// How the viewer relates to the user whose presence is requested.
#[derive(Debug, Clone, Copy, Default)]
pub struct Relationship {
    pub is_self: bool,
    pub is_friend: bool,
    pub shares_group: bool,
}

impl Relationship {
    pub const FRIEND: Self = Self {
        is_self: false,
        is_friend: true,
        shares_group: false,
    };
}

/// The visibility a user chose. Users cannot choose yet, so everyone has the default.
pub fn visibility_for(_user_id: Uuid) -> PresenceVisibility {
    DEFAULT_PRESENCE_VISIBILITY
}

pub fn can_see(visibility: PresenceVisibility, relationship: Relationship) -> bool {
    if relationship.is_self {
        return true;
    }
    match visibility {
        PresenceVisibility::Everyone => true,
        PresenceVisibility::Contacts => relationship.is_friend || relationship.shares_group,
        PresenceVisibility::Friends => relationship.is_friend,
        PresenceVisibility::Nobody => false,
    }
}

pub fn is_recent(last_seen_at: DateTime<Utc>) -> bool {
    last_seen_at > Utc::now() - Duration::seconds(ONLINE_WINDOW_SECONDS)
}

/// Whether the viewer may see that `user_id` is online right now.
pub fn is_visibly_online(
    user_id: Uuid,
    last_seen_at: DateTime<Utc>,
    relationship: Relationship,
) -> bool {
    is_recent(last_seen_at) && can_see(visibility_for(user_id), relationship)
}

/// Returns the members of one group the viewer may see online. Callers must already have checked
/// that the viewer belongs to the group, which makes every member a shared-group contact.
pub async fn visible_online_group_members(
    conn: &mut diesel_async::AsyncPgConnection,
    viewer_id: Uuid,
    member_ids: &[Uuid],
) -> Result<HashSet<Uuid>, diesel::result::Error> {
    if member_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let recent: Vec<(Uuid, DateTime<Utc>)> = user_presence::table
        .filter(user_presence::user_id.eq_any(member_ids))
        .select((user_presence::user_id, user_presence::last_seen_at))
        .load(conn)
        .await?;
    let friend_ids: HashSet<Uuid> = friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(
            friendships::requester_id
                .eq(viewer_id)
                .and(friendships::addressee_id.eq_any(member_ids))
                .or(friendships::addressee_id
                    .eq(viewer_id)
                    .and(friendships::requester_id.eq_any(member_ids))),
        )
        .select((friendships::requester_id, friendships::addressee_id))
        .load::<(Uuid, Uuid)>(conn)
        .await?
        .into_iter()
        .map(|(requester, addressee)| {
            if requester == viewer_id {
                addressee
            } else {
                requester
            }
        })
        .collect();

    Ok(recent
        .into_iter()
        .filter(|(user_id, last_seen_at)| {
            is_visibly_online(
                *user_id,
                *last_seen_at,
                Relationship {
                    is_self: *user_id == viewer_id,
                    is_friend: friend_ids.contains(user_id),
                    shares_group: true,
                },
            )
        })
        .map(|(user_id, _)| user_id)
        .collect())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{PresenceVisibility, Relationship, can_see, is_recent};

    const STRANGER: Relationship = Relationship {
        is_self: false,
        is_friend: false,
        shares_group: false,
    };
    const GROUP_CONTACT: Relationship = Relationship {
        is_self: false,
        is_friend: false,
        shares_group: true,
    };

    #[test]
    fn contacts_visibility_covers_friends_and_shared_groups_only() {
        assert!(can_see(PresenceVisibility::Contacts, Relationship::FRIEND));
        assert!(can_see(PresenceVisibility::Contacts, GROUP_CONTACT));
        assert!(!can_see(PresenceVisibility::Contacts, STRANGER));
    }

    #[test]
    fn stricter_settings_hide_presence_but_never_from_the_user_themselves() {
        assert!(!can_see(PresenceVisibility::Friends, GROUP_CONTACT));
        assert!(can_see(PresenceVisibility::Friends, Relationship::FRIEND));
        assert!(!can_see(PresenceVisibility::Nobody, Relationship::FRIEND));
        assert!(can_see(
            PresenceVisibility::Nobody,
            Relationship {
                is_self: true,
                ..STRANGER
            }
        ));
        assert!(can_see(PresenceVisibility::Everyone, STRANGER));
    }

    #[test]
    fn treats_only_recent_heartbeats_as_online() {
        assert!(is_recent(Utc::now() - Duration::seconds(10)));
        assert!(!is_recent(Utc::now() - Duration::seconds(120)));
    }
}
