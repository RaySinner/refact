import React, { useCallback, useEffect, useRef, useState } from "react";
<<<<<<< HEAD
import { Button, Code, Flex, Link, Text, TextField } from "@radix-ui/themes";
=======

>>>>>>> upstream/main
import {
  useOauthStartMutation,
  useOauthExchangeMutation,
  useOauthLogoutMutation,
  providersApi,
  capsApi,
} from "../../../services/refact";
import type {
  OAuthStartMode,
  OAuthStartResponse,
} from "../../../services/refact";
import { useAppDispatch } from "../../../hooks";
import { useOpenUrl } from "../../../hooks/useOpenUrl";
<<<<<<< HEAD

import styles from "./ProviderOAuth.module.css";

const PROVIDERS_WITH_AUTO_CALLBACK = ["openai_codex"];
=======
import { Button, FieldText, Surface } from "../../../components/ui";

import styles from "./ProviderOAuth.module.css";

const PROVIDERS_WITH_AUTO_CALLBACK = ["claude_code", "openai_codex"];
>>>>>>> upstream/main

const PROVIDER_LOGIN_LABELS: Partial<Record<string, string>> = {
  claude_code: "Login with Anthropic",
  openai_codex: "Login with OpenAI",
  github_copilot: "Login with GitHub Copilot",
};

type ProviderOAuthProps = {
  providerName: string;
  baseProvider?: string;
  oauthConnected: boolean;
  authStatus: string;
};

function inferOAuthMode(
  providerName: string,
  response: OAuthStartResponse,
): OAuthStartMode {
  if (response.mode) return response.mode;
  if (response.user_code !== undefined || providerName === "github_copilot")
    return "device";
  if (PROVIDERS_WITH_AUTO_CALLBACK.includes(providerName)) return "callback";
  return "manual_code";
}

export const ProviderOAuth: React.FC<ProviderOAuthProps> = ({
  providerName,
  baseProvider = providerName,
  oauthConnected,
  authStatus,
}) => {
  const dispatch = useAppDispatch();
  const openUrl = useOpenUrl();
  const [oauthStart] = useOauthStartMutation();
  const [oauthExchange] = useOauthExchangeMutation();
  const [oauthLogout] = useOauthLogoutMutation();

  const [sessionId, setSessionId] = useState<string | null>(null);
  const [authorizeUrl, setAuthorizeUrl] = useState<string | null>(null);
  const [oauthMode, setOauthMode] = useState<OAuthStartMode | null>(null);
  const [userCode, setUserCode] = useState<string | null>(null);
  const [instructions, setInstructions] = useState<string | null>(null);
  const [pollIntervalSeconds, setPollIntervalSeconds] = useState<number | null>(
    null,
  );
  const [deviceStatus, setDeviceStatus] = useState<string | null>(null);
  const [isDevicePolling, setIsDevicePolling] = useState(false);
  const [devicePollTick, setDevicePollTick] = useState(0);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [waitingForCallback, setWaitingForCallback] = useState(false);
  const callbackPollTimerRef = useRef<ReturnType<typeof setInterval> | null>(
    null,
  );
  const devicePollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const loginLabel = PROVIDER_LOGIN_LABELS[baseProvider] ?? "Login";
<<<<<<< HEAD
=======
  const surfaceClassName = `${styles.container} rf-stagger`;
>>>>>>> upstream/main

  const clearCallbackPollTimer = useCallback(() => {
    if (callbackPollTimerRef.current) {
      clearInterval(callbackPollTimerRef.current);
      callbackPollTimerRef.current = null;
    }
  }, []);

  const clearDevicePollTimer = useCallback(() => {
    if (devicePollTimerRef.current) {
      clearTimeout(devicePollTimerRef.current);
      devicePollTimerRef.current = null;
    }
  }, []);

  const invalidateProvider = useCallback(() => {
    dispatch(
      providersApi.util.invalidateTags([
        { type: "PROVIDER", id: providerName },
        { type: "PROVIDERS", id: "LIST" },
        { type: "AVAILABLE_MODELS", id: providerName },
      ]),
    );
  }, [dispatch, providerName]);

  const invalidateProviderAndCaps = useCallback(() => {
    invalidateProvider();
    dispatch(capsApi.util.resetApiState());
  }, [dispatch, invalidateProvider]);

  const resetOAuthState = useCallback(() => {
    setSessionId(null);
    setAuthorizeUrl(null);
    setOauthMode(null);
    setUserCode(null);
    setInstructions(null);
    setPollIntervalSeconds(null);
    setDeviceStatus(null);
    setIsDevicePolling(false);
    setDevicePollTick(0);
    setCode("");
    setWaitingForCallback(false);
    clearCallbackPollTimer();
    clearDevicePollTimer();
  }, [clearCallbackPollTimer, clearDevicePollTimer]);

  useEffect(() => {
    return () => {
      clearCallbackPollTimer();
      clearDevicePollTimer();
    };
  }, [clearCallbackPollTimer, clearDevicePollTimer]);

  const handleStartOAuth = async () => {
    setError(null);
    setIsLoading(true);
    clearCallbackPollTimer();
    clearDevicePollTimer();
    try {
      const result = await oauthStart({ providerName, mode: "max" }).unwrap();
      const mode = inferOAuthMode(baseProvider, result);
      setSessionId(result.session_id);
      setAuthorizeUrl(result.authorize_url);
      setOauthMode(mode);
      setUserCode(result.user_code ?? null);
      setInstructions(result.instructions ?? null);
      setPollIntervalSeconds(result.poll_interval ?? null);
      setDeviceStatus(null);
      setCode("");
      openUrl(result.authorize_url);

      if (mode === "callback") {
        setWaitingForCallback(true);
        callbackPollTimerRef.current = setInterval(() => {
          invalidateProvider();
        }, 2000);
      } else {
        setWaitingForCallback(false);
      }

      if (mode === "device") {
        setDeviceStatus("Waiting for device authorization");
        setIsDevicePolling(true);
        setDevicePollTick(0);
      } else {
        setIsDevicePolling(false);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to start OAuth");
    } finally {
      setIsLoading(false);
    }
  };

  const handlePollDeviceOAuth = useCallback(async () => {
    if (!sessionId) return;
    setError(null);
    setIsLoading(true);
    try {
      const result = await oauthExchange({
        providerName,
        session_id: sessionId,
        code: "",
      }).unwrap();
      if (result.success) {
        resetOAuthState();
        invalidateProviderAndCaps();
        return;
      }
      setDeviceStatus(result.auth_status || "Waiting for device authorization");
      setPollIntervalSeconds(result.poll_interval ?? pollIntervalSeconds);
      setIsDevicePolling(true);
      setDevicePollTick((tick) => tick + 1);
    } catch (e) {
      setIsDevicePolling(false);
      setError(
        e instanceof Error ? e.message : "Failed to check authorization",
      );
    } finally {
      setIsLoading(false);
    }
  }, [
    invalidateProviderAndCaps,
    oauthExchange,
    pollIntervalSeconds,
    providerName,
    resetOAuthState,
    sessionId,
  ]);

  useEffect(() => {
    if (!isDevicePolling || !sessionId) return;
    clearDevicePollTimer();
    const delaySeconds = Math.max(1, pollIntervalSeconds ?? 5);
    devicePollTimerRef.current = setTimeout(() => {
      void handlePollDeviceOAuth();
    }, delaySeconds * 1000);
    return () => {
      clearDevicePollTimer();
    };
  }, [
    clearDevicePollTimer,
    devicePollTick,
    handlePollDeviceOAuth,
    isDevicePolling,
    pollIntervalSeconds,
    sessionId,
  ]);

  useEffect(() => {
    if (waitingForCallback && oauthConnected) {
      resetOAuthState();
      invalidateProviderAndCaps();
    }
  }, [
    invalidateProviderAndCaps,
    oauthConnected,
    resetOAuthState,
    waitingForCallback,
  ]);

  useEffect(() => {
    if (!waitingForCallback) return;
    if (!authStatus) return;
    if (/failed|error|unavailable|missing/i.test(authStatus)) {
      setWaitingForCallback(false);
      clearCallbackPollTimer();
    }
  }, [authStatus, clearCallbackPollTimer, waitingForCallback]);

  const handleExchangeCode = async () => {
    if (!sessionId || !code.trim()) return;
    setError(null);
    setIsLoading(true);
    try {
      const result = await oauthExchange({
        providerName,
        session_id: sessionId,
        code: code.trim(),
      }).unwrap();
      if (!result.success) {
        setError(result.auth_status || "OAuth authorization is not complete");
        return;
      }
      resetOAuthState();
      invalidateProviderAndCaps();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to exchange code");
    } finally {
      setIsLoading(false);
    }
  };

  const handleLogout = async () => {
    setError(null);
    setIsLoading(true);
    try {
      await oauthLogout({ providerName }).unwrap();
      resetOAuthState();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to logout");
    } finally {
      setIsLoading(false);
    }
  };

  const handleCancel = () => {
    resetOAuthState();
  };

  const handleOpenAuthorizeUrl = () => {
    if (authorizeUrl) openUrl(authorizeUrl);
  };

  if (oauthConnected) {
    return (
<<<<<<< HEAD
      <Flex direction="column" gap="2" p="3" className={styles.container}>
        <Flex align="center" justify="between">
          <Flex align="center" gap="2">
            <Text size="2" weight="medium" color="green">
              ● Connected
            </Text>
            <Text size="1" color="gray">
              {authStatus}
            </Text>
          </Flex>
          <Button
            variant="ghost"
            color="red"
            size="1"
=======
      <Surface className={surfaceClassName} variant="glass" animated="rise">
        <div className={`${styles.headerRow} rf-enter-rise`}>
          <div className={styles.inlineRow}>
            <span className={`${styles.title} ${styles.connected}`}>
              Connected
            </span>
            <span className={styles.copy}>{authStatus}</span>
          </div>
          <Button
            variant="danger"
            size="sm"
>>>>>>> upstream/main
            disabled={isLoading}
            onClick={() => void handleLogout()}
          >
            Disconnect
          </Button>
<<<<<<< HEAD
        </Flex>
      </Flex>
=======
        </div>
      </Surface>
>>>>>>> upstream/main
    );
  }

  if (sessionId && authorizeUrl) {
    if (oauthMode === "device" || userCode) {
      return (
<<<<<<< HEAD
        <Flex direction="column" gap="2" p="3" className={styles.container}>
          <Text size="2" weight="medium">
=======
        <Surface className={surfaceClassName} variant="glass" animated="rise">
          <div className={`${styles.title} rf-enter-rise`}>
>>>>>>> upstream/main
            Authorize{" "}
            {(PROVIDER_LOGIN_LABELS[baseProvider] ?? "provider").replace(
              "Login with ",
              "",
            )}
<<<<<<< HEAD
          </Text>
          <Text size="1" color="gray">
            {instructions ??
              "Open the verification page and enter the code shown below."}
          </Text>
          {userCode && (
            <Flex direction="column" gap="1">
              <Text size="1" color="gray">
                User code
              </Text>
              <Code size="5">{userCode}</Code>
            </Flex>
          )}
          <Flex direction="column" gap="1">
            <Text size="1" color="gray">
              Verification URL
            </Text>
            <Link
              href={authorizeUrl}
              onClick={(e) => {
                e.preventDefault();
=======
          </div>
          <div className={`${styles.copy} rf-enter-rise`}>
            {instructions ??
              "Open the verification page and enter the code shown below."}
          </div>
          {userCode ? (
            <div className="rf-enter-rise">
              <div className={styles.copy}>User code</div>
              <div className={styles.codeBox}>{userCode}</div>
            </div>
          ) : null}
          <div className="rf-enter-rise">
            <div className={styles.copy}>Verification URL</div>
            <a
              href={authorizeUrl}
              className={styles.urlLink}
              onClick={(event) => {
                event.preventDefault();
>>>>>>> upstream/main
                handleOpenAuthorizeUrl();
              }}
            >
              {authorizeUrl}
<<<<<<< HEAD
            </Link>
          </Flex>
          <Text size="1" color="gray">
=======
            </a>
          </div>
          <div className={`${styles.copy} rf-enter-rise`}>
>>>>>>> upstream/main
            {deviceStatus ?? "Waiting for device authorization"}
            {pollIntervalSeconds
              ? ` Checking every ${pollIntervalSeconds} seconds.`
              : ""}
<<<<<<< HEAD
          </Text>
          <Flex gap="2" align="center" wrap="wrap">
            <Button variant="solid" onClick={handleOpenAuthorizeUrl}>
=======
          </div>
          <div className={`${styles.actionRow} rf-enter-rise`}>
            <Button variant="primary" onClick={handleOpenAuthorizeUrl}>
>>>>>>> upstream/main
              Open verification page
            </Button>
            <Button
              variant="soft"
              disabled={isLoading}
              onClick={() => void handlePollDeviceOAuth()}
            >
              {isLoading ? "Checking..." : "Retry"}
            </Button>
<<<<<<< HEAD
            <Button
              variant="ghost"
              size="1"
              color="gray"
              onClick={handleCancel}
            >
              Cancel
            </Button>
          </Flex>
          {error && (
            <Text size="1" color="red">
              {error}
            </Text>
          )}
        </Flex>
=======
            <Button variant="ghost" size="sm" onClick={handleCancel}>
              Cancel
            </Button>
          </div>
          {error ? (
            <div className={`${styles.errorText} rf-enter-rise`}>{error}</div>
          ) : null}
        </Surface>
>>>>>>> upstream/main
      );
    }

    if (oauthMode === "callback" && waitingForCallback) {
      return (
<<<<<<< HEAD
        <Flex direction="column" gap="2" p="3" className={styles.container}>
          <Text size="2" weight="medium">
            Waiting for authentication...
          </Text>
          <Text size="1" color="gray">
            Complete the login in the browser window that opened. This page will
            update automatically.
          </Text>
          <Flex gap="2" align="center">
            <Text size="1" color="gray">
              Browser didn&apos;t open?{" "}
              <Link
                href={authorizeUrl}
                onClick={(e) => {
                  e.preventDefault();
=======
        <Surface className={surfaceClassName} variant="glass" animated="rise">
          <div className={`${styles.title} rf-enter-rise`}>
            Waiting for authentication...
          </div>
          <div className={`${styles.copy} rf-enter-rise`}>
            Complete the login in the browser window that opened. This page will
            update automatically.
          </div>
          <div className={`${styles.actionRow} rf-enter-rise`}>
            <span className={styles.copy}>
              Browser didn&apos;t open?{" "}
              <a
                href={authorizeUrl}
                className={styles.urlLink}
                onClick={(event) => {
                  event.preventDefault();
>>>>>>> upstream/main
                  handleOpenAuthorizeUrl();
                }}
              >
                Click here
<<<<<<< HEAD
              </Link>
            </Text>
            <Button
              variant="ghost"
              size="1"
              color="gray"
              onClick={handleCancel}
            >
              Cancel
            </Button>
          </Flex>
          {error && (
            <Text size="1" color="red">
              {error}
            </Text>
          )}
        </Flex>
=======
              </a>
            </span>
            <Button variant="ghost" size="sm" onClick={handleCancel}>
              Cancel
            </Button>
          </div>
          {error ? (
            <div className={`${styles.errorText} rf-enter-rise`}>{error}</div>
          ) : null}
        </Surface>
>>>>>>> upstream/main
      );
    }

    return (
<<<<<<< HEAD
      <Flex direction="column" gap="2" p="3" className={styles.container}>
        <Text size="2" weight="medium">
          Paste the authorization code
        </Text>
        <Text size="1" color="gray">
          A browser window should have opened. Log in and copy the code shown on
          the page.
        </Text>
        <Flex gap="2">
          <TextField.Root
            className={styles.fullWidthInput}
            placeholder="Paste code here..."
            value={code}
            onChange={(e) => setCode(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void handleExchangeCode();
            }}
          />
          <Button
            variant="solid"
=======
      <Surface className={surfaceClassName} variant="glass" animated="rise">
        <div className={`${styles.title} rf-enter-rise`}>
          Paste the authorization code
        </div>
        <div className={`${styles.copy} rf-enter-rise`}>
          A browser window should have opened. Log in and copy the code shown on
          the page.
        </div>
        <div className={`${styles.actionRow} rf-enter-rise`}>
          <FieldText
            className={styles.fullWidthInput}
            placeholder="Paste code here..."
            value={code}
            onChange={setCode}
            onKeyDown={(event) => {
              if (event.key === "Enter") void handleExchangeCode();
            }}
          />
          <Button
            variant="primary"
>>>>>>> upstream/main
            disabled={isLoading || !code.trim()}
            onClick={() => void handleExchangeCode()}
          >
            {isLoading ? "Connecting..." : "Connect"}
          </Button>
<<<<<<< HEAD
        </Flex>
        <Flex gap="2" align="center">
          <Text size="1" color="gray">
            Browser didn&apos;t open?{" "}
            <Link
              href={authorizeUrl}
              onClick={(e) => {
                e.preventDefault();
=======
        </div>
        <div className={`${styles.actionRow} rf-enter-rise`}>
          <span className={styles.copy}>
            Browser didn&apos;t open?{" "}
            <a
              href={authorizeUrl}
              className={styles.urlLink}
              onClick={(event) => {
                event.preventDefault();
>>>>>>> upstream/main
                handleOpenAuthorizeUrl();
              }}
            >
              Click here
<<<<<<< HEAD
            </Link>
          </Text>
          <Button variant="ghost" size="1" color="gray" onClick={handleCancel}>
            Cancel
          </Button>
        </Flex>
        {error && (
          <Text size="1" color="red">
            {error}
          </Text>
        )}
      </Flex>
=======
            </a>
          </span>
          <Button variant="ghost" size="sm" onClick={handleCancel}>
            Cancel
          </Button>
        </div>
        {error ? (
          <div className={`${styles.errorText} rf-enter-rise`}>{error}</div>
        ) : null}
      </Surface>
>>>>>>> upstream/main
    );
  }

  return (
<<<<<<< HEAD
    <Flex direction="column" gap="2" p="3" className={styles.container}>
      <Flex align="center" justify="between">
        <Text size="2" weight="medium">
          {loginLabel}
        </Text>
        <Button
          variant="solid"
=======
    <Surface className={surfaceClassName} variant="glass" animated="rise">
      <div className={`${styles.headerRow} rf-enter-rise`}>
        <div className={styles.title}>{loginLabel}</div>
        <Button
          variant="primary"
>>>>>>> upstream/main
          disabled={isLoading}
          onClick={() => void handleStartOAuth()}
        >
          {isLoading ? "Starting..." : "Login"}
        </Button>
<<<<<<< HEAD
      </Flex>
      {error && (
        <Text size="1" color="red">
          {error}
        </Text>
      )}
    </Flex>
=======
      </div>
      {error ? (
        <div className={`${styles.errorText} rf-enter-rise`}>{error}</div>
      ) : null}
    </Surface>
>>>>>>> upstream/main
  );
};
