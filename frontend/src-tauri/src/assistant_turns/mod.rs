//! Вопросы ассистенту одной встречи (Konspekt). Пишутся в транзакции
//! сохранения встречи из журнала записи; question и answer шифруются.

pub mod commands;

use sqlx::{SqliteConnection, SqlitePool};

use crate::claude_session::AssistantTurn;
use crate::database::fields;

pub async fn store(conn: &mut SqliteConnection, meeting_id: &str, turns: &[AssistantTurn]) -> Result<(), sqlx::Error> {
    for turn in turns {
        sqlx::query(
            "INSERT INTO assistant_turns (id, meeting_id, asked_at_sec, question, answer, model, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO NOTHING",
        )
        .bind(&turn.id)
        .bind(meeting_id)
        .bind(turn.asked_at_sec)
        .bind(fields::seal(fields::ASSISTANT_QUESTION, &turn.question)?)
        .bind(fields::seal(fields::ASSISTANT_ANSWER, &turn.answer)?)
        .bind(&turn.model)
        .bind(&turn.created_at)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub async fn load(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<AssistantTurn>, sqlx::Error> {
    let rows: Vec<(String, f64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, asked_at_sec, question, answer, model, created_at FROM assistant_turns \
         WHERE meeting_id = ? ORDER BY asked_at_sec, created_at",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(id, asked_at_sec, question, answer, model, created_at)| {
            Ok(AssistantTurn {
                id,
                asked_at_sec,
                question: fields::open(fields::ASSISTANT_QUESTION, &question)?,
                answer: fields::open(fields::ASSISTANT_ANSWER, &answer)?,
                model,
                created_at,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool_with_meeting() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::database::manager::MIGRATOR.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 't', '2026-10-10', '2026-10-10')")
            .execute(&pool).await.unwrap();
        pool
    }

    fn turn(id: &str, at: f64) -> AssistantTurn {
        AssistantTurn { id: id.into(), asked_at_sec: at, question: format!("вопрос {id}"), answer: "ответ **жирный**".into(), model: "haiku".into(), created_at: "2026-10-10T09:00:00Z".into() }
    }

    #[tokio::test]
    async fn turns_come_back_in_the_order_they_were_asked() {
        let pool = pool_with_meeting().await;
        let mut conn = pool.acquire().await.unwrap();
        store(&mut conn, "m1", &[turn("b", 90.0), turn("a", 30.0)]).await.unwrap();
        drop(conn);
        let loaded = load(&pool, "m1").await.unwrap();
        assert_eq!(loaded.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(loaded[0].question, "вопрос a");
        assert_eq!(load(&pool, "other").await.unwrap(), vec![]);
    }
}
