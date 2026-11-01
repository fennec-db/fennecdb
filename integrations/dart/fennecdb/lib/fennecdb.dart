/// fennecdb embedded in a Dart or Flutter app: a database in a file on the
/// device, through the native library (crates/fennec-ffi) --
/// FennecQL or the query builder, and live queries as Streams.
library;

export 'src/fennec.dart'
    show
        Fennec,
        FennecException,
        FennecCode,
        Answer,
        Rows,
        FacetCount,
        Facets,
        Changes,
        Shape,
        SyncState,
        SyncStatus,
        SyncFailure,
        ShapeState,
        Refusal,
        Replica,
        SchemaPlan,
        SchemaRefusal,
        SchemaException,
        FennecSchema;
export 'src/remote.dart' show BatchAnswer, FennecRemote, Reads;
export 'src/live.dart' show LiveQueries;
export 'src/query.dart' show Query, Call, Cond, Computed, Param, SortKey, Statement;
