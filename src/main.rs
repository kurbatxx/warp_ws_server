use futures::StreamExt;
use sqlx::migrate::MigrateDatabase;
use sqlx::sqlite::SqlitePoolOptions;
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

static NEXT_USERID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>>;

#[tokio::main]
async fn main() {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.1.127:5009".to_string());
    let socket_address: SocketAddr = addr.parse().expect("unvalid socket address");

    //BD
    let uri = "sqlite://data.db";
    let _ = sqlx::Sqlite::create_database(uri).await;
    let pool = SqlitePoolOptions::new().connect(uri).await.unwrap();

    let _create_users_table = sqlx::query(
        "create table if not exists message (
    id integer primary key autoincrement,
    message_text text)",
    )
    .execute(&pool)
    .await;

    //pool.close().await;

    //Server
    let users = Users::default();
    let users = warp::any().map(move || users.clone());

    let opt = warp::path::param::<String>()
        .map(Some)
        .or_else(|_| async { Ok::<(Option<String>,), std::convert::Infallible>((None,)) });

    // GET /hello/warp => 200 OK with body "Hello, warp!"
    let hello = warp::path("hello")
        .and(opt)
        .and(warp::path::end())
        .map(|name: Option<String>| {
            format!("Hello, {}!", name.unwrap_or_else(|| "world".to_string()))
        });

    // GET /ws
    let chat = warp::path("ws")
        .and(warp::ws())
        .and(users)
        .map(|ws: warp::ws::Ws, users| ws.on_upgrade(move |socket| connect(socket, users)));

    let files = warp::fs::dir("./static");

    let res_404 = warp::any().map(|| {
        warp::http::Response::builder()
            .status(warp::http::StatusCode::NOT_FOUND)
            .body(fs::read_to_string("./static/404.html").expect("404 404?"))
    });

    let routes = chat.or(hello).or(files).or(res_404);

    let server = warp::serve(routes).try_bind(socket_address);

    println!("Running server at {}!", addr);

    server.await
}

async fn connect(ws: WebSocket, users: Users) {
    // Bookkeeping
    let my_id = NEXT_USERID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    println!("Welcome User {}", my_id);

    // Establishing a connection
    let (user_tx, mut user_rx) = ws.split();
    let (tx, rx) = mpsc::unbounded_channel();

    let rx = UnboundedReceiverStream::new(rx);

    tokio::spawn(rx.forward(user_tx));

    //
    tx.send(Ok(Message::text("Start"))).expect("Какая-то лажа");
    //

    users.write().await.insert(my_id, tx);

    // Reading and broadcasting messages
    while let Some(result) = user_rx.next().await {
        if result.is_ok() {
            broadcast_msg(result.expect("Failed to fetch message"), &users).await;
        } else {
            break;
        }
    }

    // Disconnect
    disconnect(my_id, &users).await;
}

async fn broadcast_msg(msg: Message, users: &Users) {
    if let Ok(_) = msg.to_str() {
        for (&_uid, tx) in users.read().await.iter() {
            tx.send(Ok(msg.clone())).expect("Failed to send message");
        }
    }
}

async fn disconnect(my_id: usize, users: &Users) {
    println!("Good bye user {}", my_id);
    users.write().await.remove(&my_id);
}
