use fake::{faker::name::en::Name, Fake};
use serde_json::Value;

use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use sqlx::migrate::MigrateDatabase;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::FromRow;
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::str::FromStr;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

enum ActionEnum {
    GetMessages,
    AddMessage,
    DeleteMessage,
    UpdateStatus,
}

impl FromStr for ActionEnum {
    type Err = ();

    fn from_str(input: &str) -> Result<ActionEnum, Self::Err> {
        match input {
            "get_messages" => Ok(ActionEnum::GetMessages),
            "add_message" => Ok(ActionEnum::AddMessage),
            "delete_message" => Ok(ActionEnum::DeleteMessage),
            "update_status" => Ok(ActionEnum::UpdateStatus),
            _ => Err(()),
        }
    }
}

impl ActionEnum {
    pub fn value(self) -> String {
        match self {
            ActionEnum::GetMessages => "get_messages".to_string(),
            ActionEnum::AddMessage => "add_message".to_string(),
            ActionEnum::DeleteMessage => "delete_message".to_string(),
            ActionEnum::UpdateStatus => "update_status".to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
struct DbMessage {
    id: i64,
    message: String,
    status: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Action<T> {
    action: String,
    data: T,
}

static NEXT_USERID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>>;

static URI: &str = "sqlite://data.db";

#[tokio::main]
async fn main() {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.1.127:5009".to_string());
    let socket_address: SocketAddr = addr.parse().expect("unvalid socket address");

    //BD
    let _ = sqlx::Sqlite::create_database(URI).await;
    let pool = SqlitePoolOptions::new().connect(URI).await.unwrap();

    let _create_task_table = sqlx::query(
        "create table if not exists message (
    id integer primary key autoincrement,
    message_text text,
    status varchar(12) default wait not null)",
    )
    .execute(&pool)
    .await;

    // let mut stream = tokio_stream::iter(vec![0; 1000]);
    // while let Some(_row) = stream.next().await {
    //     _ = sqlx::query(
    //         "INSERT INTO message (message_text)
    //         VALUES ($1)",
    //     )
    //     .bind(&(Name().fake::<String>()))
    //     .execute(&pool)
    //     .await;
    // }

    pool.close().await;

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

    // DB --
    let pool = SqlitePoolOptions::new().connect(URI).await.unwrap();
    let select_query = sqlx::query_as::<_, DbMessage>(
        "SELECT id, message_text as message, status 
        FROM message",
    );
    let messages: Vec<DbMessage> = select_query.fetch_all(&pool).await.unwrap();

    let event = Action {
        action: "get_messages".to_string(),
        data: messages,
    };

    let result = serde_json::to_string(&event).unwrap();
    dbg!(&result);

    tx.send(Ok(Message::text(result))).expect("Какая-то лажа");
    pool.close().await;
    //

    users.write().await.insert(my_id, tx);

    // Reading and broadcasting messages
    while let Some(result) = user_rx.next().await {
        if result.is_ok() {
            let in_message = result.unwrap();
            let text = in_message.to_str().unwrap();
            dbg!(text);
            let json_value: Value = serde_json::from_str(text).unwrap();
            let result = json_value.get("action").unwrap().to_string();
            let action = str::replace(&result, "\"", "");
            dbg!(&action);
            match ActionEnum::from_str(action.as_str()) {
                Ok(action_enum) => match action_enum {
                    ActionEnum::GetMessages => {}
                    ActionEnum::AddMessage => {
                        let action: Action<String> = serde_json::from_str(text).unwrap();
                        let pool = SqlitePoolOptions::new().connect(URI).await.unwrap();
                        let row: Result<DbMessage, sqlx::Error> = sqlx::query_as(
                            "insert into message (message_text) values ($1) returning id, message_text as message, status",
                        )
                        .bind(action.data)
                        .fetch_one(&pool)
                        .await;

                        pool.close().await;

                        match row {
                            Ok(message) => {
                                let act = Action {
                                    action: action_enum.value(),
                                    data: message,
                                };
                                let msg = serde_json::to_string(&act).unwrap();
                                broadcast_msg(Message::text(msg), &users).await;
                            }
                            Err(err) => {
                                dbg!(err);
                            }
                        }
                    }
                    ActionEnum::DeleteMessage => {
                        let action: Action<DbMessage> = serde_json::from_str(text).unwrap();
                        let id = action.data.id;
                        dbg!(id);
                        let pool = SqlitePoolOptions::new().connect(URI).await.unwrap();
                        let row: Result<DbMessage, sqlx::Error> =
                            sqlx::query_as("delete from message where message.id = ($1) returning id, message_text as message, status")
                                .bind(id)
                                .fetch_one(&pool)
                                .await;

                        pool.close().await;

                        match row {
                            Ok(message) => {
                                let act = Action {
                                    action: action_enum.value(),
                                    data: message,
                                };
                                let msg = serde_json::to_string(&act).unwrap();
                                broadcast_msg(Message::text(msg), &users).await;
                            }
                            Err(err) => {
                                dbg!(err);
                            }
                        }
                    }
                    ActionEnum::UpdateStatus => {
                        let action: Action<DbMessage> = serde_json::from_str(text).unwrap();
                        let id = action.data.id;
                        dbg!(id);
                        let status = action.data.status;
                        dbg!(&status);

                        let pool = SqlitePoolOptions::new().connect(URI).await.unwrap();
                        let row: Result<DbMessage, sqlx::Error> =
                            sqlx::query_as("update message set status = ($2) where message.id = ($1) returning id, message_text as message, status")
                                .bind(id)
                                .bind(status)
                                .fetch_one(&pool)
                                .await;

                        pool.close().await;

                        match row {
                            Ok(message) => {
                                let act = Action {
                                    action: action_enum.value(),
                                    data: message,
                                };
                                let msg = serde_json::to_string(&act).unwrap();
                                broadcast_msg(Message::text(msg), &users).await;
                            }
                            Err(err) => {
                                dbg!(err);
                            }
                        }
                    }
                },
                Err(err) => {
                    dbg!(err);
                }
            };
            //broadcast_msg(result.expect("Failed to fetch message"), &users).await;
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
