export interface ColumnMeta {
  isColumnBuilder: boolean;
  type: string;
  name?: string;
  nullable: boolean;
  primaryKey: boolean;
  autoIncrement: boolean;
  unique: boolean;
  defaultVal?: any;
  maxLength?: number;
  refThunk?: () => ColumnRef;
  onDelete?: string;
  onUpdate?: string;
}

export interface ColumnRef {
  $isColumnRef: true;
  $table: any;
  $tableName: string;
  $key: string;
  $name: string;
}

export declare class ColumnBuilder {
  readonly meta: ColumnMeta;
  constructor(meta: ColumnMeta);
  name(name: string): ColumnBuilder;
  nullable(): ColumnBuilder;
  primaryKey(opts?: { autoIncrement?: boolean }): ColumnBuilder;
  autoIncrement(): ColumnBuilder;
  unique(): ColumnBuilder;
  maxLength(len: number): ColumnBuilder;
  default(val: any): ColumnBuilder;
  defaultNow(): ColumnBuilder;
  references(refThunk: () => ColumnRef, opts?: { onDelete?: string; onUpdate?: string }): ColumnBuilder;
}

export declare function integer(): ColumnBuilder;
export declare function bigint(): ColumnBuilder;
export declare function float(): ColumnBuilder;
export declare function double(): ColumnBuilder;
export declare function text(): ColumnBuilder;
export declare function boolean(): ColumnBuilder;
export declare function timestamp(): ColumnBuilder;
export declare function timestamptz(): ColumnBuilder;
export declare function uuid(): ColumnBuilder;
export declare function json(): ColumnBuilder;
export declare function enumType(name: string, values: string[]): () => ColumnBuilder;

export declare function table(name: string, columns: Record<string, ColumnBuilder>, extraFn?: (helpers: any) => any[]): any;
export declare function table(columns: Record<string, ColumnBuilder>, extraFn?: (helpers: any) => any[]): any;

export interface DefineSchemaOptions {
  enums?: any[];
  tables: Record<string, any> | any[];
}

export declare function defineSchema(options: DefineSchemaOptions): any;
