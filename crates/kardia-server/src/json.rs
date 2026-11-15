use kardia_core::Result;
use serde::Serialize;
use serde_json::Value;

pub(crate) fn stringify(value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value)?;
    let mut output = String::new();
    write(&value, &mut output)?;
    Ok(output)
}
fn write(value: &Value, output: &mut String) -> Result<()> {
    match value {
        Value::Number(number) => {
            if let Some(number) = number.as_f64().filter(|number| number.is_finite()) {
                output.push_str(ryu_js::Buffer::new().format(number));
            } else {
                output.push_str("null");
            }
        }
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write(item, output)?;
            }
            output.push(']');
        }
        Value::Object(items) => {
            let mut entries: Vec<_> = items.iter().collect();
            entries.sort_by_key(|(key, _)| array_index(key).unwrap_or(u32::MAX));
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                write(value, output)?;
            }
            output.push('}');
        }
        _ => output.push_str(&serde_json::to_string(value)?),
    }
    Ok(())
}
fn array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}
