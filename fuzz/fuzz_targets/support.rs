use brook::ty::Ty;
use brook::{Engine, Script, Value};

pub fn exercise(engine: &Engine, script: &Script) {
    for (index, func) in script.code.funcs.iter().take(8).enumerate() {
        let args = func
            .params
            .iter()
            .enumerate()
            .map(|(param, ty)| sample_value(ty, index * 11 + param, 0))
            .collect::<Option<Vec<_>>>();
        if let Some(args) = args {
            let _ = engine.call(script, &func.name, args);
        }
    }
}

fn sample_value(ty: &Ty, salt: usize, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    match ty {
        Ty::I64 => Some(Value::I64((salt % 7) as i64 - 3)),
        Ty::F64 => Some(Value::F64((salt % 5) as f64 - 2.0)),
        Ty::Bool => Some(Value::Bool(salt % 2 == 0)),
        Ty::Str => Some(Value::from(if salt % 2 == 0 { "fuzz" } else { "" })),
        Ty::Unit => Some(Value::Unit),
        Ty::Tuple(items) => items
            .iter()
            .map(|item| sample_value(item, salt + 1, depth + 1))
            .collect::<Option<Vec<Value>>>()
            .map(Value::Tuple),
        Ty::Arr(_) => Some(Value::arr(Vec::new())),
        Ty::Handle(_) | Ty::Var(_) => None,
    }
}
