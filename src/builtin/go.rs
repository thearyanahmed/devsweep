use crate::sweep::{Registry, Safety, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    // Often committed on purpose; git decides when it is.
    r.add(
        SimpleSweeper::new("go")
            .folders(&["vendor"])
            .markers(&["go.mod"])
            .rebuild("go mod vendor")
            .safety(Safety::Caution),
    );
}
