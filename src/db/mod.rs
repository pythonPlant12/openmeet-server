use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;

// Type alias for our async PostgreSQL connection pool
pub type DbPool = Pool<AsyncPgConnection>;

// Creates a connection pool from DATABASE_URL
// Pool pre-creates connections for fast reuse across requests
pub fn create_pool(database_url: &str) -> DbPool {
    let config = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder(config)
        .build()
        .expect("Failed to create database pool")
}
