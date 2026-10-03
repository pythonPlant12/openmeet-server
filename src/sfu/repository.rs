use crate::sfu::room::Room;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Trait for room storage - allows easy swapping of implementations
/// (in-memory now, Firebase/database later)
#[async_trait]
pub trait RoomRepository: Send + Sync {
    /// Create a new room
    async fn create_room(&self, room_id: String) -> Result<()>;

    /// Get a room by ID (returns None if not found)
    async fn get_room(&self, room_id: &str) -> Option<Arc<RwLock<Room>>>;

    /// Delete a room
    async fn delete_room(&self, room_id: &str) -> Result<()>;

    /// Permanently revoke a room for this process and remove it from active storage.
    async fn revoke_room(&self, room_id: &str) -> Result<Option<Arc<RwLock<Room>>>>;

    /// Deny a user without waiting for the room's async lock.
    async fn deny_room_user(&self, room_id: &str, user_id: Uuid) -> Result<()>;

    /// Check if room exists
    async fn room_exists(&self, room_id: &str) -> bool;

    /// Get all room IDs
    async fn list_rooms(&self) -> Vec<String>;
}

pub struct InMemoryRoomRepository {
    state: Arc<RwLock<RepositoryState>>,
}

#[derive(Default)]
struct RepositoryState {
    rooms: HashMap<String, RoomEntry>,
    revoked_room_ids: HashSet<String>,
}

struct RoomEntry {
    room: Arc<RwLock<Room>>,
    revoked: Arc<AtomicBool>,
    denied_user_ids: Arc<std::sync::RwLock<HashSet<Uuid>>>,
}

impl InMemoryRoomRepository {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(RepositoryState::default())),
        }
    }
}

#[async_trait]
impl RoomRepository for InMemoryRoomRepository {
    async fn create_room(&self, room_id: String) -> Result<()> {
        let mut state = self.state.write().await;

        if state.revoked_room_ids.contains(&room_id) {
            return Err(anyhow::anyhow!("Room {} is revoked", room_id));
        }
        if state.rooms.contains_key(&room_id) {
            return Err(anyhow::anyhow!("Room {} already exists", room_id));
        }

        let room = Room::new(room_id.clone());
        let revoked = room.revocation_flag();
        let denied_user_ids = room.denied_user_ids();
        state.rooms.insert(
            room_id,
            RoomEntry {
                room: Arc::new(RwLock::new(room)),
                revoked,
                denied_user_ids,
            },
        );

        Ok(())
    }

    async fn get_room(&self, room_id: &str) -> Option<Arc<RwLock<Room>>> {
        let state = self.state.read().await;
        state
            .rooms
            .get(room_id)
            .map(|entry| Arc::clone(&entry.room))
    }

    async fn delete_room(&self, room_id: &str) -> Result<()> {
        let mut state = self.state.write().await;
        state.rooms.remove(room_id);
        Ok(())
    }

    async fn revoke_room(&self, room_id: &str) -> Result<Option<Arc<RwLock<Room>>>> {
        let mut state = self.state.write().await;
        state.revoked_room_ids.insert(room_id.to_string());
        let entry = state.rooms.remove(room_id);
        if let Some(entry) = &entry {
            entry.revoked.store(true, Ordering::Release);
        }
        Ok(entry.map(|entry| entry.room))
    }

    async fn deny_room_user(&self, room_id: &str, user_id: Uuid) -> Result<()> {
        let denied_user_ids = {
            let state = self.state.read().await;
            state
                .rooms
                .get(room_id)
                .map(|entry| Arc::clone(&entry.denied_user_ids))
        };
        if let Some(denied_user_ids) = denied_user_ids {
            denied_user_ids
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(user_id);
        }
        Ok(())
    }

    async fn room_exists(&self, room_id: &str) -> bool {
        let state = self.state.read().await;
        state.rooms.contains_key(room_id)
    }

    async fn list_rooms(&self) -> Vec<String> {
        let state = self.state.read().await;
        state.rooms.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_get_room() {
        let repo = InMemoryRoomRepository::new();

        repo.create_room("room123".to_string()).await.unwrap();

        assert!(repo.room_exists("room123").await);
        assert!(!repo.room_exists("room456").await);

        let room = repo.get_room("room123").await;
        assert!(room.is_some());

        let room_lock = room.unwrap();
        let room = room_lock.read().await;
        assert_eq!(room.id, "room123");
    }

    #[tokio::test]
    async fn test_delete_room() {
        let repo = InMemoryRoomRepository::new();

        repo.create_room("room123".to_string()).await.unwrap();
        assert!(repo.room_exists("room123").await);

        repo.delete_room("room123").await.unwrap();
        assert!(!repo.room_exists("room123").await);
    }

    #[tokio::test]
    async fn test_list_rooms() {
        let repo = InMemoryRoomRepository::new();

        repo.create_room("room1".to_string()).await.unwrap();
        repo.create_room("room2".to_string()).await.unwrap();
        repo.create_room("room3".to_string()).await.unwrap();

        let rooms = repo.list_rooms().await;
        assert_eq!(rooms.len(), 3);
        assert!(rooms.contains(&"room1".to_string()));
        assert!(rooms.contains(&"room2".to_string()));
        assert!(rooms.contains(&"room3".to_string()));
    }

    #[tokio::test]
    async fn revoked_room_is_removed_marked_and_cannot_be_recreated() {
        let repo = InMemoryRoomRepository::new();
        repo.create_room("managed-room".to_string()).await.unwrap();
        let existing_reference = repo.get_room("managed-room").await.unwrap();

        let revoked_room = repo.revoke_room("managed-room").await.unwrap().unwrap();

        assert!(Arc::ptr_eq(&existing_reference, &revoked_room));
        assert!(existing_reference.read().await.is_revoked());
        assert!(!repo.room_exists("managed-room").await);
        assert!(repo.get_room("managed-room").await.is_none());
        assert!(repo.create_room("managed-room".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn revoking_missing_room_prevents_later_creation() {
        let repo = InMemoryRoomRepository::new();

        assert!(repo.revoke_room("deleted-room").await.unwrap().is_none());
        assert!(repo.create_room("deleted-room".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn denying_user_does_not_wait_for_room_write_lock() {
        let repo = InMemoryRoomRepository::new();
        let denied_user_id = Uuid::new_v4();
        repo.create_room("managed-room".to_string()).await.unwrap();
        let room_lock = repo.get_room("managed-room").await.unwrap();
        let room = room_lock.write().await;

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            repo.deny_room_user("managed-room", denied_user_id),
        )
        .await
        .expect("denial must not wait for the room write lock")
        .unwrap();

        assert!(room.is_user_denied(denied_user_id));
    }
}
