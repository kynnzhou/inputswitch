import tseslint from "typescript-eslint";

// 新代码全是 TS，直接上严格档；不设 suppressions，有问题当场修。
export default tseslint.config(
  ...tseslint.configs.recommended,
  {
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
);
