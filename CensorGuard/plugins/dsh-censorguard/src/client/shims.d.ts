// 浏览器半的最小环境类型声明 (本包编译期不引入 react/cordis 依赖,
// 运行时这些 specifier 由 DSH ModuleLoader 的平台模块表解析, 见
// deepseek-harness packages/client/web/src/platform.ts 的 PLATFORM_MODULES)

declare module 'react' {
  export type ReactNode = unknown;
  export type CSSProperties = Record<string, string | number | undefined>;
  export type FC<P = Record<string, never>> = (props: P) => unknown;
  export function useState<T>(init: T | (() => T)): [T, (v: T | ((p: T) => T)) => void];
  export function useEffect(fn: () => void | (() => void), deps?: unknown[]): void;
  export function useMemo<T>(fn: () => T, deps: unknown[]): T;
  export function useCallback<T>(fn: T, deps: unknown[]): T;
  export function useSyncExternalStore<T>(
    subscribe: (cb: () => void) => () => void,
    getSnapshot: () => T,
  ): T;
  const React: Record<string, unknown>;
  export default React;
}

declare module 'react/jsx-runtime' {
  export namespace JSX {
    type Element = unknown;
    interface IntrinsicElements {
      [elemName: string]: Record<string, unknown>;
    }
    interface IntrinsicAttributes {
      key?: string | number | null;
    }
    interface ElementChildrenAttribute {
      children: unknown;
    }
  }
  export function jsx(type: unknown, props: unknown, key?: unknown): unknown;
  export function jsxs(type: unknown, props: unknown, key?: unknown): unknown;
  export const Fragment: unknown;
}
