/// JavaScript-facing DSL wrapper functions in JavaScript / TypeScript.
/// Exported so tests and scripts can import either directly from `@alloy-ts/repository/sql`
/// or use the pure JS/TS bindings.
export function toSnakeCase(s: string): string {
  return s
    .replace(/([a-z\d])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z\d]+)/g, "$1_$2")
    .toLowerCase()
    .replace(/[-.\s]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

const VALID_ACTIONS = new Set(["no_action", "restrict", "cascade", "set_null", "set_default"]);

export class ColumnBuilder {
  meta: any;

  constructor(meta: any = {}) {
    this.meta = Object.freeze({
      name: meta.name || null,
      type: meta.type || null,
      nullable: meta.nullable ?? false,
      autoIncrement: meta.autoIncrement ?? false,
      maxLength: meta.maxLength ?? null,
      default: meta.default !== undefined ? meta.default : undefined,
      unique: meta.unique ?? false,
      primaryKey: meta.primaryKey ?? false,
      references: meta.references ? { ...meta.references } : null,
      isColumnBuilder: true,
    });
    Object.freeze(this);
  }

  name(name: string) {
    return new ColumnBuilder({ ...this.meta, name });
  }

  nullable() {
    return new ColumnBuilder({ ...this.meta, nullable: true });
  }

  primaryKey(opts: any = {}) {
    return new ColumnBuilder({
      ...this.meta,
      primaryKey: true,
      autoIncrement: opts.autoIncrement ? true : this.meta.autoIncrement,
    });
  }

  unique() {
    return new ColumnBuilder({ ...this.meta, unique: true });
  }

  maxLength(len: number) {
    if (typeof len !== "number" || !Number.isInteger(len) || len <= 0) {
      throw new Error("maxLength expects a positive integer");
    }
    return new ColumnBuilder({ ...this.meta, maxLength: len });
  }

  default(val: any) {
    const def = val === null ? { kind: "null" } : { kind: "literal", value: val };
    return new ColumnBuilder({ ...this.meta, default: def });
  }

  defaultNow() {
    return new ColumnBuilder({ ...this.meta, default: { kind: "now" } });
  }

  references(thunk: any, opts: any = {}) {
    if (typeof thunk !== "function") {
      throw new Error("references() expects a thunk (() => table.column)");
    }
    const onDelete = opts.onDelete ? toSnakeCase(opts.onDelete) : "no_action";
    const onUpdate = opts.onUpdate ? toSnakeCase(opts.onUpdate) : "no_action";

    if (!VALID_ACTIONS.has(onDelete) || !VALID_ACTIONS.has(onUpdate)) {
      throw new Error("unknown referential action");
    }

    return new ColumnBuilder({
      ...this.meta,
      references: { thunk, onDelete, onUpdate },
    });
  }
}

export const integer = () => new ColumnBuilder({ type: { kind: "integer" } });
export const bigint = () => new ColumnBuilder({ type: { kind: "bigint" } });
export const float = () => new ColumnBuilder({ type: { kind: "float" } });
export const double = () => new ColumnBuilder({ type: { kind: "double" } });
export const text = () => new ColumnBuilder({ type: { kind: "text" } });
export const boolean = () => new ColumnBuilder({ type: { kind: "boolean" } });
export const timestamp = () => new ColumnBuilder({ type: { kind: "timestamp" } });
export const timestamptz = () => new ColumnBuilder({ type: { kind: "timestamptz" } });
export const uuid = () => new ColumnBuilder({ type: { kind: "uuid" } });
export const json = () => new ColumnBuilder({ type: { kind: "json" } });

export function enumType(name: string, values: string[]) {
  const seen = new Set<string>();
  for (const v of values) {
    if (seen.has(v)) {
      throw new Error(`enumType("${name}") duplicate values: "${v}"`);
    }
    seen.add(v);
  }

  const factory = () => new ColumnBuilder({ type: { kind: "enum", name } });
  (factory as any)._enumDef = { name, values: [...values] };
  return factory;
}

export function table(tableName: string, columnsObj: Record<string, any>, extraFn: any = null) {
  for (const [key, val] of Object.entries(columnsObj)) {
    if (key.startsWith("$")) {
      throw new Error(`reserved key "${key}"`);
    }
    if (!val || typeof val !== "object" || !val.meta?.isColumnBuilder) {
      throw new Error(`expected a column builder for key "${key}"`);
    }
  }

  if (extraFn) {
    const helpers = {
      primaryKey: (cols: string[]) => ({ kind: "primaryKey", columns: cols }),
      index: (cols: string[], opts: any = {}) => ({
        kind: "index",
        columns: cols,
        name: opts?.name,
      }),
      unique: (cols: string[], opts: any = {}) => ({
        kind: "unique",
        columns: cols,
        name: opts?.name,
      }),
    };
    const extraItems = extraFn(helpers);
    if (!Array.isArray(extraItems)) {
      throw new Error("table() third argument must return an array");
    }
  }

  const tableObj: any = {
    $tableName: tableName,
    $columnsObj: columnsObj,
    $extraFn: extraFn,
  };

  for (const [key, builder] of Object.entries(columnsObj)) {
    const colName = builder.meta.name || toSnakeCase(key);
    tableObj[key] = {
      $isColumnRef: true,
      $table: tableObj,
      $tableName: tableName,
      $key: key,
      $name: colName,
    };
  }

  return tableObj;
}

export function defineSchema({ enums = [], tables = [] }: any) {
  const tableNames = new Set<string>();
  for (const t of tables) {
    if (tableNames.has(t.$tableName)) {
      throw new Error(`duplicate table names: "${t.$tableName}"`);
    }
    tableNames.add(t.$tableName);
  }

  const registeredTables = new Set(tables.map((t: any) => t.$tableName));
  const enumList = enums.map((e: any) => e._enumDef);

  const irTables = tables.map((t: any) => {
    const tableName = t.$tableName;
    const columnsObj = t.$columnsObj;
    const extraFn = t.$extraFn;

    let pkDeclaredOnCol = false;
    const colPkNames: string[] = [];
    const colUniques: any[] = [];
    const foreignKeys: any[] = [];

    const keyToColName = new Map<string, string>();
    for (const [key, builder] of Object.entries(columnsObj)) {
      const colName = (builder as any).meta.name || toSnakeCase(key);
      keyToColName.set(key, colName);
    }

    const columns: any[] = [];
    for (const [key, builder] of Object.entries(columnsObj)) {
      const colName = keyToColName.get(key)!;
      const b = (builder as any).meta;

      if (b.primaryKey) {
        pkDeclaredOnCol = true;
        colPkNames.push(colName);
      }
      if (b.unique) {
        colUniques.push({
          name: `${tableName}_${colName}_key`,
          columns: [colName],
        });
      }

      if (b.references) {
        const ref = b.references;
        const target = ref.thunk();
        if (!target || typeof target !== "object" || !target.$isColumnRef) {
          throw new Error(`references() must return a column (e.g. () => table.column)`);
        }
        if (!registeredTables.has(target.$tableName)) {
          throw new Error(`target table "${target.$tableName}" is not in defineSchema`);
        }

        foreignKeys.push({
          name: `${tableName}_${colName}_fkey`,
          columns: [colName],
          refTable: target.$tableName,
          refColumns: [target.$name],
          onDelete: ref.onDelete,
          onUpdate: ref.onUpdate,
        });
      }

      const colDef: any = {
        key,
        name: colName,
        type: b.type,
        nullable: b.nullable,
        autoIncrement: b.autoIncrement,
      };
      if (b.maxLength !== null) {
        colDef.maxLength = b.maxLength;
      }
      if (b.default !== undefined) {
        colDef.default = b.default;
      }
      columns.push(colDef);
    }

    const tableColsSet = new Set(columns.map((c) => c.name));
    let tableLevelPk = null;
    const indexes: any[] = [];
    const uniques = [...colUniques];

    if (extraFn) {
      const helpers = {
        primaryKey: (cols: string[]) => ({ kind: "primaryKey", columns: cols }),
        index: (cols: string[], opts: any = {}) => ({
          kind: "index",
          columns: cols,
          name: opts?.name,
        }),
        unique: (cols: string[], opts: any = {}) => ({
          kind: "unique",
          columns: cols,
          name: opts?.name,
        }),
      };
      const extraItems = extraFn(helpers);

      for (const item of extraItems) {
        if (item.kind === "primaryKey") {
          if (pkDeclaredOnCol) {
            throw new Error(`table "${tableName}": primary key declared twice`);
          }
          tableLevelPk = item.columns.map((colKey: string) => {
            const mapped = keyToColName.get(colKey) || colKey;
            if (!tableColsSet.has(mapped)) {
              throw new Error(`unknown column "${colKey}" in primaryKey`);
            }
            return mapped;
          });
        } else if (item.kind === "index") {
          const mappedCols = item.columns.map((colKey: string) => {
            const mapped = keyToColName.get(colKey) || colKey;
            if (!tableColsSet.has(mapped)) {
              throw new Error(`unknown column "${colKey}" in index`);
            }
            return mapped;
          });
          const idxName = item.name || `${tableName}_${mappedCols.join("_")}_idx`;
          indexes.push({ name: idxName, columns: mappedCols });
        } else if (item.kind === "unique") {
          const mappedCols = item.columns.map((colKey: string) => {
            const mapped = keyToColName.get(colKey) || colKey;
            if (!tableColsSet.has(mapped)) {
              throw new Error(`unknown column "${colKey}" in unique`);
            }
            return mapped;
          });
          const unqName = item.name || `${tableName}_${mappedCols.join("_")}_key`;
          uniques.push({ name: unqName, columns: mappedCols });
        }
      }
    }

    const primaryKey = tableLevelPk || colPkNames;

    return {
      name: tableName,
      columns,
      primaryKey,
      uniques,
      indexes,
      foreignKeys,
    };
  });

  return {
    irVersion: 1,
    enums: enumList,
    tables: irTables,
  };
}
