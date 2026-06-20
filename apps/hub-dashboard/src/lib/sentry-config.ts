export type SentryEnv = Record<string, string | undefined>;

export type DashboardSentryOptions = {
  dsn: string;
  environment?: string;
  release?: string;
  sendDefaultPii: false;
  tracesSampleRate: 0;
};

export function getBrowserSentryOptions(env: SentryEnv): DashboardSentryOptions | null {
  const dsn = nonEmpty(env.NEXT_PUBLIC_SENTRY_DSN);
  if (!dsn) {
    return null;
  }
  return buildOptions(dsn, env);
}

export function getServerSentryOptions(env: SentryEnv): DashboardSentryOptions | null {
  const dsn = nonEmpty(env.SENTRY_DSN) ?? nonEmpty(env.NEXT_PUBLIC_SENTRY_DSN);
  if (!dsn) {
    return null;
  }
  return buildOptions(dsn, env);
}

function buildOptions(dsn: string, env: SentryEnv): DashboardSentryOptions {
  return {
    dsn,
    environment: nonEmpty(env.SENTRY_ENVIRONMENT),
    release: nonEmpty(env.SENTRY_RELEASE),
    sendDefaultPii: false,
    tracesSampleRate: 0,
  };
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}
