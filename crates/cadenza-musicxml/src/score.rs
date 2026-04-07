use cadenza_theory::phrase::Phrase;

pub struct ScoreMetadata {
    pub title: String,
    pub composer: String,
    pub copyright: String,
}

pub struct Part {
    pub id: String,
    pub name: String,
    pub phrase: Phrase,
}

pub struct Score {
    pub metadata: ScoreMetadata,
    pub parts: Vec<Part>,
}
