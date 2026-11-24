use boa_engine::{Context, JsResult, JsValue, Source};
pub struct EcmaScriptEngine {
    context: Context,
}
impl EcmaScriptEngine {
    pub fn new() -> Self {
        Self {
            context: Context::default(),
        }
    }
    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }
    pub fn evaluate(&mut self, source: &str) -> JsResult<JsValue> {
        self.context.eval(Source::from_bytes(source))
    }
    pub fn evaluate_to_string(&mut self, source: &str) -> JsResult<String> {
        let value = self.evaluate(source)?;
        Ok(value.to_string(&mut self.context)?.to_std_string_escaped())
    }
}
impl Default for EcmaScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}
