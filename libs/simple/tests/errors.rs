use std::fmt::Debug;

use zerogc_derive::Trace;

use zerogc::array::GcString;
use zerogc::prelude::*;
use zerogc_simple::{CollectorId, Gc, SimpleCollector, SimpleCollectorContext as GcContext};

#[derive(Debug, thiserror::Error, Trace)]
#[zerogc(collector_ids(CollectorId))]
pub enum OurError<'gc> {
    #[error("Bad gc string: {0}")]
    BadGcString(GcString<'gc, CollectorId>),
    #[error("Bad gc int: {0}")]
    BadGcInt(Gc<'gc, i32>),
    #[error("Bad non-gc string: {0}")]
    BadOtherString(String),
}

fn implicitly_alloc<'gc>(ctx: &'gc GcContext, val: i32) -> Result<String, OurError<'gc>> {
    match val {
        0 => Err(OurError::BadGcString(ctx.alloc_str("gc foo"))),
        1 => Err(OurError::BadOtherString(String::from("boxed foo"))),
        2 => Err(OurError::BadGcInt(ctx.alloc(15))),
        _ => Ok(String::from("sensible result")),
    }
}

fn into_anyhow<'gc>(ctx: &'gc GcContext, val: i32) -> Result<String, anyhow::Error> {
    let s = implicitly_alloc(ctx, val).map_err(|e| ctx.alloc_error(e))?;
    Ok(format!("Result: {}", s))
}

#[test]
fn test_errors() {
    let collector = SimpleCollector::create();
    let ctx = collector.create_context();
    fn display_anyhow(e: anyhow::Error) -> String {
        format!("{}", e)
    }
    assert_eq!(
        into_anyhow(&ctx, 0).map_err(display_anyhow),
        Err("Bad gc string: gc foo".into())
    );
    assert_eq!(
        into_anyhow(&ctx, 1).map_err(display_anyhow),
        Err("Bad non-gc string: boxed foo".into())
    );
    assert_eq!(
        into_anyhow(&ctx, 2).map_err(display_anyhow),
        Err("Bad gc int: 15".into())
    );
    assert_eq!(
        into_anyhow(&ctx, 3).map_err(display_anyhow),
        Ok("Result: sensible result".into())
    );
}
