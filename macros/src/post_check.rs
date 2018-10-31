use syn::parse;

use analyze::Ownerships;
use syntax::App;

// TODO remove?
pub fn ownerships(_app: &App, _ownerships: &Ownerships) -> parse::Result<()> {
    // for (resource, ownership) in ownerships {
    //     if !ownership.is_owned() && app.resources[resource].singleton {
    //         return Err(parse::Error::new(
    //             resource.span(),
    //             "Wrapping singletons in Mutexes is currently not supported",
    //         ));
    //     }
    // }

    Ok(())
}
