/// CamelCase or mixed identifier to snake_case converter
export function toSnakeCase(s) {
  return s
    .replace(/([a-z\d])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z\d]+)/g, "$1_$2")
    .toLowerCase()
    .replace(/[-.\s]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

const VALID_ACTIONS = new Set(["no_action", "restrict", "cascade", "set_null", "set_default"]);

class ColumnBuilder {
  constructor(meta = {}) {
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

  name(name) {
    return new ColumnBuilder({ ...this.meta, name });
  }

  nullable() {
    return new ColumnBuilder({ ...this.meta, nullable: true });
  }

  primaryKey(opts = {}) {
    return new ColumnBuilder({
      ...this.meta,
      primaryKey: true,
      autoIncrement: opts.autoIncrement ? true : this.meta.autoIncrement,
    });
  }

  unique() {
    return new ColumnBuilder({ ...this.meta, unique: true });
  }

  maxLength(len) {
    if (typeof len !== "number" || !Number.isInteger(len) || len <= 0) {
      throw new Error("maxLength expects a positive integer");
    }
    return new ColumnBuilder({ ...this.meta, maxLength: len });
  }

  default(val) {
    const def = val === null ? { kind: "null" } : { kind: "literal", value: val };
    return new ColumnBuilder({ ...this.meta, default: def });
  }

  defaultNow() {
    return new ColumnBuilder({ ...this.meta, default: { kind: "now" } });
  }

  references(thunk, opts = {}) {
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

export function enumType(name, values) {
  const seen = new Set();
  for (const v of values) {
    if (seen.has(v)) {
      throw new Error(`enumType("${name}") duplicate values: "${v}"`);
    }
    seen.add(v);
  }

  const factory = () => new ColumnBuilder({ type: { kind: "enum", name } });
  factory._enumDef = { name, values: [...values] };
  return factory;
}

export function table(tableName, columnsObj, extraFn = null) {
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
      primaryKey: (cols) => ({ kind: "primaryKey", columns: cols }),
      index: (cols, opts = {}) => ({ kind: "index", columns: cols, name: opts?.name }),
      unique: (cols, opts = {}) => ({ kind: "unique", columns: cols, name: opts?.name }),
    };
    const extraItems = extraFn(helpers);
    if (!Array.isArray(extraItems)) {
      throw new Error("table() third argument must return an array");
    }
  }

  const tableObj = {
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

export function defineSchema({ enums = [], tables = [] }) {
  const tableNames = new Set();
  for (const t of tables) {
    if (tableNames.has(t.$tableName)) {
      throw new Error(`duplicate table names: "${t.$tableName}"`);
    }
    tableNames.add(t.$tableName);
  }

  const registeredTables = new Set(tables.map((t) => t.$tableName));
  const enumList = enums.map((e) => e._enumDef);

  const irTables = tables.map((t) => {
    const tableName = t.$tableName;
    const columnsObj = t.$columnsObj;
    const extraFn = t.$extraFn;

    let pkDeclaredOnCol = false;
    const colPkNames = [];
    const colUniques = [];
    const foreignKeys = [];

    const keyToColName = new Map();
    for (const [key, builder] of Object.entries(columnsObj)) {
      const colName = builder.meta.name || toSnakeCase(key);
      keyToColName.set(key, colName);
    }

    const columns = [];
    for (const [key, builder] of Object.entries(columnsObj)) {
      const colName = keyToColName.get(key);

      if (builder.meta.primaryKey) {
        pkDeclaredOnCol = true;
        colPkNames.push(colName);
      }
      if (builder.meta.unique) {
        colUniques.push({
          name: `${tableName}_${colName}_key`,
          columns: [colName],
        });
      }

      if (builder.meta.references) {
        const ref = builder.meta.references;
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

      const colDef = {
        key,
        name: colName,
        type: builder.meta.type,
        nullable: builder.meta.nullable,
        autoIncrement: builder.meta.autoIncrement,
      };
      if (builder.meta.maxLength !== null) {
        colDef.maxLength = builder.meta.maxLength;
      }
      if (builder.meta.default !== undefined) {
        colDef.default = builder.meta.default;
      }
      columns.push(colDef);
    }

    const tableColsSet = new Set(columns.map((c) => c.name));
    let tableLevelPk = null;
    const indexes = [];
    const uniques = [...colUniques];

    if (extraFn) {
      const helpers = {
        primaryKey: (cols) => ({ kind: "primaryKey", columns: cols }),
        index: (cols, opts = {}) => ({ kind: "index", columns: cols, name: opts.name }),
        unique: (cols, opts = {}) => ({ kind: "unique", columns: cols, name: opts.name }),
      };
      const extraItems = extraFn(helpers);
      if (!Array.isArray(extraItems)) {
        throw new Error("table() extraFn must return an array");
      }

      for (const item of extraItems) {
        if (item.kind === "primaryKey") {
          if (pkDeclaredOnCol) {
            throw new Error(`table "${tableName}": primary key declared twice`);
          }
          tableLevelPk = item.columns.map((colKey) => {
            const mapped = keyToColName.get(colKey) || colKey;
            if (!tableColsSet.has(mapped)) {
              throw new Error(`unknown column "${colKey}" in primaryKey`);
            }
            return mapped;
          });
        } else if (item.kind === "index") {
          const mappedCols = item.columns.map((colKey) => {
            const mapped = keyToColName.get(colKey) || colKey;
            if (!tableColsSet.has(mapped)) {
              throw new Error(`unknown column "${colKey}" in index`);
            }
            return mapped;
          });
          const idxName = item.name || `${tableName}_${mappedCols.join("_")}_idx`;
          indexes.push({ name: idxName, columns: mappedCols });
        } else if (item.kind === "unique") {
          const mappedCols = item.columns.map((colKey) => {
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
