use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use crate::sfu::room::Room;

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

    /// Check if room exists
    async fn room_exists(&self, room_id: &str) -> bool;

    /// Get all room IDs
    async fn list_rooms(&self) -> Vec<String>;
}

pub struct InMemoryRoomRepository {
    rooms: Arc<RwLock<HashMap<String, Arc<RwLock<Room>>>>>,
}

impl InMemoryRoomRepository {
    pub fn new() -> Self {
        Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl RoomRepository for InMemoryRoomRepository {
    async fn create_room(&self, room_id: String) -> Result<()> {
        let mut rooms = self.rooms.write().await;

        if rooms.contains_key(&room_id) {
            return Err(anyhow::anyhow!("Room {} already exists", room_id));
        }

        let room = Room::new(room_id.clone());
        rooms.insert(room_id, Arc::new(RwLock::new(room)));

        Ok(())
    }

    async fn get_room(&self, room_id: &str) -> Option<Arc<RwLock<Room>>> {
        let rooms = self.rooms.read().await;
        rooms.get(room_id).cloned()
    }

    async fn delete_room(&self, room_id: &str) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        rooms.remove(room_id);
        Ok(())
    }

    async fn room_exists(&self, room_id: &str) -> bool {
        let rooms = self.rooms.read().await;
        rooms.contains_key(room_id)
    }

    async fn list_rooms(&self) -> Vec<String> {
        let rooms = self.rooms.read().await;
        rooms.keys().cloned().collect()
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
}
