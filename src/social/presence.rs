//! Online presence, user statuses, and who may see them. Every endpoint that reports `isOnline` or a
//! status goes through this module, so a per-user privacy setting only needs to change `visibility_for`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use crate::{
    schema::{friendships, user_presence},
    social::models::UserStatus,
};

/// Clients heartbeat presence more often than this, so a newer timestamp means the user is online.
const ONLINE_WINDOW_SECONDS: i64 = 45;

/// Who can see a user's online state and status.
#[allow(dead_code)] // Everyone, Contacts, and Nobody become reachable once users pick a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceVisibility {
    Everyone,
    /// Accepted friends and people who share a group with the user.
    Contacts,
    Friends,
    Nobody,
}

/// Statuses are personal, so only accepted friends see them until users can choose otherwise.
pub const DEFAULT_PRESENCE_VISIBILITY: PresenceVisibility = PresenceVisibility::Friends;

/// How the viewer relates to the user whose presence is requested.
#[derive(Debug, Clone, Copy, Default)]
pub struct Relationship {
    pub is_self: bool,
    pub is_friend: bool,
    pub shares_group: bool,
    /// Both people are connected to the same live meeting right now.
    pub shares_meeting: bool,
}

impl Relationship {
    pub const FRIEND: Self = Self {
        is_self: false,
        is_friend: true,
        shares_group: false,
        shares_meeting: false,
    };
    pub const SELF: Self = Self {
        is_self: true,
        is_friend: false,
        shares_group: false,
        shares_meeting: false,
    };
    /// People in the same live meeting see each other's status on their video tiles.
    pub const MEETING_PEER: Self = Self {
        is_self: false,
        is_friend: false,
        shares_group: false,
        shares_meeting: true,
    };
}

/// A user's status and online state as one viewer is allowed to see them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisiblePresence {
    pub status: UserStatus,
    pub is_online: bool,
}

/// The visibility a user chose. Users cannot choose yet, so everyone has the default.
pub fn visibility_for(_user_id: Uuid) -> PresenceVisibility {
    DEFAULT_PRESENCE_VISIBILITY
}

pub fn can_see(visibility: PresenceVisibility, relationship: Relationship) -> bool {
    if relationship.is_self {
        return true;
    }
    // A shared live meeting already shows who is there, so only users who hide from everyone stay hidden.
    if relationship.shares_meeting {
        return visibility != PresenceVisibility::Nobody;
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

/// Returns what the viewer may see, or `None` when the user's presence is hidden from them.
/// "Appear offline" hides online state from everyone else but the user still sees their own.
pub fn visible_presence(
    user_id: Uuid,
    status: UserStatus,
    last_seen_at: Option<DateTime<Utc>>,
    relationship: Relationship,
) -> Option<VisiblePresence> {
    if !can_see(visibility_for(user_id), relationship) {
        return None;
    }
    let connected = last_seen_at.is_some_and(is_recent);
    let is_online = connected && (relationship.is_self || status != UserStatus::Offline);
    Some(VisiblePresence { status, is_online })
}

/// Presence of one group's members as the viewer may see it. Callers must already have checked
/// that the viewer belongs to the group, which makes every member a shared-group contact.
pub async fn visible_group_member_presence(
    conn: &mut diesel_async::AsyncPgConnection,
    viewer_id: Uuid,
    members: &[(Uuid, UserStatus)],
) -> Result<HashMap<Uuid, VisiblePresence>, diesel::result::Error> {
    if members.is_empty() {
        return Ok(HashMap::new());
    }
    let member_ids = members.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let last_seen: HashMap<Uuid, DateTime<Utc>> = user_presence::table
        .filter(user_presence::user_id.eq_any(&member_ids))
        .select((user_presence::user_id, user_presence::last_seen_at))
        .load::<(Uuid, DateTime<Utc>)>(conn)
        .await?
        .into_iter()
        .collect();
    let friend_ids = accepted_friend_ids(conn, viewer_id, &member_ids).await?;

    Ok(members
        .iter()
        .filter_map(|(user_id, status)| {
            visible_presence(
                *user_id,
                *status,
                last_seen.get(user_id).copied(),
                Relationship {
                    is_self: *user_id == viewer_id,
                    is_friend: friend_ids.contains(user_id),
                    shares_group: true,
                    shares_meeting: false,
                },
            )
            .map(|presence| (*user_id, presence))
        })
        .collect())
}

async fn accepted_friend_ids(
    conn: &mut diesel_async::AsyncPgConnection,
    viewer_id: Uuid,
    candidate_ids: &[Uuid],
) -> Result<HashSet<Uuid>, diesel::result::Error> {
    Ok(friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(
            friendships::requester_id
                .eq(viewer_id)
                .and(friendships::addressee_id.eq_any(candidate_ids))
                .or(friendships::addressee_id
                    .eq(viewer_id)
                    .and(friendships::requester_id.eq_any(candidate_ids))),
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
        .collect())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    use super::{
        PresenceVisibility, Relationship, VisiblePresence, can_see, is_recent, visible_presence,
    };
    use crate::social::models::UserStatus;

    const STRANGER: Relationship = Relationship {
        is_self: false,
        is_friend: false,
        shares_group: false,
        shares_meeting: false,
    };
    const GROUP_CONTACT: Relationship = Relationship {
        is_self: false,
        is_friend: false,
        shares_group: true,
        shares_meeting: false,
    };

    #[test]
    fn shares_presence_only_with_friends_by_default() {
        let now = Some(Utc::now());
        let user = Uuid::new_v4();

        assert_eq!(
            visible_presence(user, UserStatus::Away, now, Relationship::FRIEND),
            Some(VisiblePresence {
                status: UserStatus::Away,
                is_online: true
            })
        );
        assert_eq!(
            visible_presence(user, UserStatus::Away, now, GROUP_CONTACT),
            None
        );
        assert_eq!(
            visible_presence(user, UserStatus::Away, now, STRANGER),
            None
        );
    }

    #[test]
    fn appear_offline_hides_online_state_from_others_only() {
        let now = Some(Utc::now());
        let user = Uuid::new_v4();

        assert!(
            !visible_presence(user, UserStatus::Offline, now, Relationship::FRIEND)
                .unwrap()
                .is_online
        );
        assert!(
            visible_presence(user, UserStatus::Offline, now, Relationship::SELF)
                .unwrap()
                .is_online
        );
    }

    #[test]
    fn visibility_settings_cover_each_audience() {
        assert!(can_see(PresenceVisibility::Contacts, GROUP_CONTACT));
        assert!(!can_see(PresenceVisibility::Contacts, STRANGER));
        assert!(!can_see(PresenceVisibility::Friends, GROUP_CONTACT));
        assert!(!can_see(PresenceVisibility::Nobody, Relationship::FRIEND));
        assert!(can_see(PresenceVisibility::Nobody, Relationship::SELF));
        assert!(can_see(PresenceVisibility::Everyone, STRANGER));
    }

    #[test]
    fn meeting_peers_see_status_unless_the_user_hides_from_everyone() {
        assert!(can_see(
            PresenceVisibility::Friends,
            Relationship::MEETING_PEER
        ));
        assert!(can_see(
            PresenceVisibility::Contacts,
            Relationship::MEETING_PEER
        ));
        assert!(!can_see(
            PresenceVisibility::Nobody,
            Relationship::MEETING_PEER
        ));
    }

    #[test]
    fn treats_only_recent_heartbeats_as_online() {
        assert!(is_recent(Utc::now() - Duration::seconds(10)));
        assert!(!is_recent(Utc::now() - Duration::seconds(120)));
        assert!(
            !visible_presence(
                Uuid::new_v4(),
                UserStatus::Available,
                None,
                Relationship::FRIEND
            )
            .unwrap()
            .is_online
        );
    }
}
