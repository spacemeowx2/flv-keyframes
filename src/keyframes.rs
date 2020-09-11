#[derive(Debug, Clone)]
pub struct Keyframes {
    filepositions: Vec<f64>,
    times: Vec<f64>,
    pub offset: f64,
}
impl Keyframes {
    pub fn new() -> Keyframes {
        Keyframes {
            filepositions: vec![],
            times: vec![],
            offset: 0f64,
        }
    }
    pub fn add(&mut self, offset: u64, time: f64) {
        self.filepositions.push(offset as f64);
        self.times.push(time);
    }
    pub fn into_amf0(self) -> (String, amf::amf0::Value) {
        use amf::{amf0::{self, Value}, Pair};
        let offset = self.offset;
        let keyframes = Value::Object {
            class_name: None,
            entries: vec![
                Pair {
                    key: "filepositions".to_string(),
                    value: amf0::array(
                        self.filepositions
                            .into_iter()
                            .map(|i| i + offset)
                            .map(amf0::number)
                            .collect()
                    )
                },
                Pair {
                    key: "times".to_string(),
                    value: amf0::array(
                        self.times
                            .into_iter()
                            .map(amf0::number)
                            .collect()
                    )
                }
            ],
        };
        (
            "keyframes".to_string(),
            keyframes,
        )
    }
}
