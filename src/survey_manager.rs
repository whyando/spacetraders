use crate::database::DbClient;
use crate::models::{KeyedSurvey, Survey, WaypointSymbol};
use chrono::Duration;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub struct SurveyManager {
    db: DbClient,
    inner: Mutex<SurveyManagerInner>,
}

struct SurveyManagerInner {
    surveys: BTreeMap<WaypointSymbol, Vec<KeyedSurvey>>,
}

impl SurveyManager {
    pub async fn new(db: &DbClient) -> Self {
        let surveys = db.get_surveys().await;
        let surveys = surveys
            .into_iter()
            .fold(BTreeMap::new(), |mut map, survey| {
                map.entry(survey.survey.symbol.clone())
                    .or_insert_with(Vec::new)
                    .push(survey);
                map
            });
        Self {
            db: db.clone(),
            inner: Mutex::new(SurveyManagerInner { surveys }),
        }
    }

    pub async fn insert_surveys(&self, surveys: Vec<Survey>) {
        let surveys: Vec<KeyedSurvey> = surveys
            .into_iter()
            .map(|survey| KeyedSurvey {
                uuid: uuid::Uuid::new_v4(),
                survey,
            })
            .collect();
        self.db.insert_surveys(&surveys).await;
        let mut inner = self.inner.lock().unwrap();
        for survey in surveys {
            inner
                .surveys
                .entry(survey.survey.symbol.clone())
                .or_default()
                .push(survey);
        }
    }

    fn survey_score(&self, survey: &Survey) -> f64 {
        let mut score = 0.0;
        for deposit in &survey.deposits {
            score += match deposit.symbol.as_str() {
                // FAB_MATS:
                "IRON_ORE" => 1.0,
                "QUARTZ_SAND" => 1.0,
                // ADVANCED CIRCUITS
                "COPPER_ORE" => 1.0,
                "SILICON_CRYSTALS" => 1.0,
                // USELESS
                "ALUMINUM_ORE" => 0.0,
                "ICE_WATER" => 0.0,
                _ => panic!("Unexpected deposit symbol: {}", deposit.symbol),
            };
        }
        score / survey.deposits.len() as f64
    }

    pub async fn get_survey(&self, waypoint: &WaypointSymbol) -> Option<KeyedSurvey> {
        let now = chrono::Utc::now();
        loop {
            // grab front
            let best = {
                let mut inner = self.inner.lock().unwrap();
                let surveys = inner.surveys.entry(waypoint.clone()).or_default();
                surveys.sort_by(|a, b| {
                    self.survey_score(&a.survey)
                        .partial_cmp(&self.survey_score(&b.survey))
                        .unwrap()
                });
                surveys.last().cloned()
            };
            // delete or return
            if let Some(survey) = best {
                if survey.survey.expiration + Duration::try_minutes(5).unwrap() < now {
                    self.remove_survey(&survey).await;
                } else {
                    return Some(survey.clone());
                }
            } else {
                return None;
            }
        }
    }

    pub async fn remove_survey(&self, survey: &KeyedSurvey) {
        log::debug!("Deleting survey {}", survey.uuid);
        self.db.remove_survey(&survey.uuid).await;

        let mut inner = self.inner.lock().unwrap();
        inner
            .surveys
            .entry(survey.survey.symbol.clone())
            .and_modify(|v| {
                v.retain(|s| s.uuid != survey.uuid);
            });
    }
}
