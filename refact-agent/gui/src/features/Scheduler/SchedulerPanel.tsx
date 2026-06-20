import React, { useMemo, useState } from "react";
<<<<<<< HEAD
import { Button, Card, Flex, Heading, Text } from "@radix-ui/themes";
import { ArrowLeftIcon, ReloadIcon } from "@radix-ui/react-icons";
import { useAppSelector } from "../../hooks";
import {
  type CreateCronRequest,
=======
import { ArrowLeft, RefreshCw } from "lucide-react";
import {
  Badge,
  Button,
  FieldError,
  StatusDot,
  Surface,
} from "../../components/ui";
import { useAppSelector } from "../../hooks";
import {
  type CreateCronRequest,
  type UpdateCronRequest,
>>>>>>> upstream/main
  schedulerErrorMessage,
  useCreateCronMutation,
  useDeleteCronMutation,
  useGetCronTasksQuery,
<<<<<<< HEAD
=======
  useRunCronMutation,
  useUpdateCronMutation,
>>>>>>> upstream/main
} from "../../services/refact/schedulerApi";
import {
  selectCurrentThreadId,
  selectThreadMode,
} from "../Chat/Thread/selectors";
<<<<<<< HEAD
=======
import { SettingsSection } from "../Settings/SettingsSection";
>>>>>>> upstream/main
import { CronCreateForm } from "./CronCreateForm";
import { selectLastCronFireAt } from "./schedulerSlice";
import { CronList } from "./CronList";
import styles from "./Scheduler.module.css";

type SchedulerPanelProps = {
  onBack: () => void;
<<<<<<< HEAD
};

export const SchedulerPanel: React.FC<SchedulerPanelProps> = ({ onBack }) => {
=======
  embedded?: boolean;
};

type CronListUpdate = Omit<UpdateCronRequest, "id">;

export const SchedulerPanel: React.FC<SchedulerPanelProps> = ({
  onBack,
  embedded,
}) => {
>>>>>>> upstream/main
  const {
    data: tasks = [],
    isFetching,
    error,
    refetch,
  } = useGetCronTasksQuery(undefined);
  const [createCron, createState] = useCreateCronMutation();
  const [deleteCron, deleteState] = useDeleteCronMutation();
<<<<<<< HEAD
  const [deletingId, setDeletingId] = useState<string | null>(null);
=======
  const [updateCron, updateState] = useUpdateCronMutation();
  const [runCron, runState] = useRunCronMutation();
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [updatingId, setUpdatingId] = useState<string | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<unknown>(null);
  const [updateError, setUpdateError] = useState<unknown>(null);
  const [runError, setRunError] = useState<unknown>(null);
>>>>>>> upstream/main
  const lastCronFireAt = useAppSelector(selectLastCronFireAt);
  const currentThreadId = useAppSelector(selectCurrentThreadId);
  const currentMode = useAppSelector(selectThreadMode);

<<<<<<< HEAD
=======
  const recurringCount = tasks.filter((task) => task.recurring).length;
  const durableCount = tasks.filter((task) => task.durable).length;
  const enabledCount = tasks.filter((task) => task.enabled).length;

>>>>>>> upstream/main
  const sortedTasks = useMemo(
    () =>
      [...tasks].sort((left, right) =>
        left.next_fire_at_ms === right.next_fire_at_ms
          ? left.id.localeCompare(right.id)
          : left.next_fire_at_ms - right.next_fire_at_ms,
      ),
    [tasks],
  );
<<<<<<< HEAD
=======
  const renderedMutationError =
    deleteState.error ??
    updateState.error ??
    runState.error ??
    deleteError ??
    updateError ??
    runError;
>>>>>>> upstream/main

  const handleCreate = async (
    request: Omit<CreateCronRequest, "chat_id" | "mode">,
  ) => {
    await createCron({
      ...request,
      chat_id: currentThreadId,
      mode: currentMode ?? undefined,
    }).unwrap();
  };

  const handleDelete = async (id: string) => {
    setDeletingId(id);
<<<<<<< HEAD
    try {
      await deleteCron({ id }).unwrap();
=======
    setDeleteError(null);
    try {
      await deleteCron({ id }).unwrap();
    } catch (err) {
      setDeleteError(err);
>>>>>>> upstream/main
    } finally {
      setDeletingId(null);
    }
  };

<<<<<<< HEAD
=======
  const handleUpdate = async (id: string, request: CronListUpdate) => {
    setUpdatingId(id);
    setUpdateError(null);
    try {
      await updateCron({ id, ...request }).unwrap();
    } catch (err) {
      setUpdateError(err);
    } finally {
      setUpdatingId(null);
    }
  };

  const handleRunNow = async (id: string) => {
    setRunningId(id);
    setRunError(null);
    try {
      await runCron({ id }).unwrap();
    } catch (err) {
      setRunError(err);
    } finally {
      setRunningId(null);
    }
  };

>>>>>>> upstream/main
  const deleteTask = (id: string) => {
    void handleDelete(id);
  };

<<<<<<< HEAD
  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <Button variant="outline" onClick={onBack}>
          <ArrowLeftIcon width="16" height="16" />
          Back
        </Button>
        <Heading size="5">⏰ Scheduler</Heading>
        <Button variant="soft" onClick={() => void refetch()}>
          <ReloadIcon width="16" height="16" />
          Refresh
        </Button>
      </div>
      <div className={styles.content}>
        <CronCreateForm
          onSubmit={handleCreate}
          isLoading={createState.isLoading}
          error={createState.error}
          taskCount={tasks.length}
        />
        <Card>
          <Flex direction="column" gap="3">
            <Flex justify="between" align="center">
              <Text size="4" weight="bold">
                Scheduled prompts
              </Text>
              {lastCronFireAt && (
                <Text size="1" color="gray">
                  Last fired {new Date(lastCronFireAt).toLocaleTimeString()}
                </Text>
              )}
            </Flex>
            {error && (
              <Text className={styles.error} role="alert" size="2">
                {schedulerErrorMessage(error)}
              </Text>
            )}
            {deleteState.error && (
              <Text className={styles.error} role="alert" size="2">
                {schedulerErrorMessage(deleteState.error)}
              </Text>
            )}
=======
  const toggleEnabled = (id: string, enabled: boolean) => {
    void handleUpdate(id, { enabled });
  };

  const runNow = (id: string) => {
    void handleRunNow(id);
  };

  const updateTask = (id: string, request: CronListUpdate) => {
    void handleUpdate(id, request);
  };

  const actions = (
    <>
      {!embedded && (
        <Button variant="soft" onClick={onBack} leftIcon={ArrowLeft}>
          Back
        </Button>
      )}
      <Button
        variant="soft"
        onClick={() => void refetch()}
        leftIcon={RefreshCw}
      >
        Refresh
      </Button>
    </>
  );

  const summary = (
    <div className={styles.summaryBadges} aria-label="Scheduler summary">
      <Badge tone="default" variant="glass">
        <StatusDot status="idle" />
        {tasks.length} total
      </Badge>
      <Badge tone="success" variant="glass">
        <StatusDot status="success" />
        {enabledCount} enabled
      </Badge>
      <Badge tone="success" variant="glass">
        <StatusDot status="success" />
        {recurringCount} recurring
      </Badge>
      <Badge tone="accent" variant="glass">
        <StatusDot status="running" />
        {durableCount} durable
      </Badge>
      {lastCronFireAt ? (
        <Badge tone="muted" variant="glass">
          Last fired {new Date(lastCronFireAt).toLocaleTimeString()}
        </Badge>
      ) : null}
    </div>
  );

  return (
    <SettingsSection
      title="Scheduler"
      description="Create, review, and manage scheduled prompts for the current chat."
      actions={actions}
      subNav={summary}
    >
      <div className={styles.panel}>
        <div className={styles.layout}>
          <Surface
            className={styles.createCard}
            variant="glass"
            animated="rise"
          >
            <h3 className={styles.paneTitle}>New schedule</h3>
            <CronCreateForm
              onSubmit={handleCreate}
              isLoading={createState.isLoading}
              error={createState.error}
              taskCount={tasks.length}
            />
          </Surface>

          <section
            className={styles.listPane}
            aria-labelledby="scheduler-list-title"
          >
            <div className={styles.listHeader}>
              <div className={styles.listTitleBlock}>
                <h3 className={styles.paneTitle} id="scheduler-list-title">
                  Scheduled prompts
                </h3>
                <p className={styles.sectionHint}>
                  Review lifecycle status, next and last fire times, schedule
                  scope, recurrence, and prompt description.
                </p>
              </div>
            </div>
            {error ? (
              <FieldError>{schedulerErrorMessage(error)}</FieldError>
            ) : null}
            {renderedMutationError ? (
              <FieldError>
                {schedulerErrorMessage(renderedMutationError)}
              </FieldError>
            ) : null}
>>>>>>> upstream/main
            <CronList
              tasks={sortedTasks}
              isLoading={isFetching}
              deletingId={deletingId}
<<<<<<< HEAD
              onDelete={deleteTask}
            />
          </Flex>
        </Card>
      </div>
    </div>
=======
              updatingId={updatingId}
              runningId={runningId}
              onDelete={deleteTask}
              onToggleEnabled={toggleEnabled}
              onRunNow={runNow}
              onUpdate={updateTask}
            />
          </section>
        </div>
      </div>
    </SettingsSection>
>>>>>>> upstream/main
  );
};
