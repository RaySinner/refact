import React, { useCallback, useState } from "react";
import {
<<<<<<< HEAD
  Flex,
  Box,
  Text,
  Button,
  Card,
  Badge,
  TextField,
  TextArea,
  Heading,
  Spinner,
} from "@radix-ui/themes";
import {
  ArrowLeftIcon,
  PlusIcon,
  DotFilledIcon,
  CheckCircledIcon,
  CrossCircledIcon,
  LayersIcon,
  PauseIcon,
} from "@radix-ui/react-icons";
import { ChatLoading } from "../../components/ChatContent/ChatLoading";
import { ScrollArea } from "../../components/ScrollArea";
import { CloseButton } from "../../components/Buttons/Buttons";
=======
  ArrowLeft,
  CheckCircle,
  Circle,
  CircleX,
  Layers,
  LoaderCircle,
  Pause,
  Plus,
  Trash2,
} from "lucide-react";
import { ChatLoading } from "../../components/ChatContent/ChatLoading";
import {
  Button,
  Card,
  FieldTextarea,
  FieldText,
  Icon,
  IconButton,
  Badge,
} from "../../components/ui";
>>>>>>> upstream/main
import { useAppDispatch } from "../../hooks";
import { pop, push } from "../Pages/pagesSlice";
import {
  useListTasksQuery,
  useCreateTaskMutation,
  useDeleteTaskMutation,
  TaskMeta,
} from "../../services/refact/tasks";
import { openTask } from "./tasksSlice";
<<<<<<< HEAD

const statusColors: Record<
  TaskMeta["status"],
  "gray" | "blue" | "yellow" | "green" | "red"
> = {
  planning: "gray",
  active: "blue",
  paused: "yellow",
  completed: "green",
  abandoned: "red",
};
=======
import styles from "./Tasks.module.css";
>>>>>>> upstream/main

const statusLabels: Record<TaskMeta["status"], string> = {
  planning: "Planning",
  active: "Active",
  paused: "Paused",
  completed: "Done",
  abandoned: "Abandoned",
};

<<<<<<< HEAD
=======
const statusTones: Record<
  TaskMeta["status"],
  React.ComponentProps<typeof Badge>["tone"]
> = {
  planning: "muted",
  active: "accent",
  paused: "warning",
  completed: "success",
  abandoned: "danger",
};

>>>>>>> upstream/main
interface TaskItemProps {
  task: TaskMeta;
  onClick: () => void;
  onDelete: () => void;
}

<<<<<<< HEAD
const TaskItem: React.FC<TaskItemProps> = ({ task, onClick, onDelete }) => {
  const dateUpdated = new Date(task.updated_at);
  const dateTimeString = dateUpdated.toLocaleString();
  const plannerState = task.planner_session_state;
=======
function taskStatusIcon(
  task: TaskMeta,
  plannerState: TaskMeta["planner_session_state"],
) {
>>>>>>> upstream/main
  const isPlannerWorking =
    plannerState === "generating" || plannerState === "executing_tools";
  const isPlannerPaused =
    plannerState === "paused" || plannerState === "waiting_ide";
<<<<<<< HEAD
  const isPlannerError = plannerState === "error";
  const isCompleted = task.status === "completed";
  const isFailed = task.status === "abandoned";

  return (
    <Box style={{ position: "relative", width: "100%" }}>
      <Card
        style={{ width: "100%", marginBottom: "2px" }}
        variant="surface"
        className="rt-Button"
        asChild
        role="button"
      >
        <button
          onClick={(event) => {
            event.preventDefault();
            event.stopPropagation();
            onClick();
          }}
        >
          <Flex gap="1" align="center">
            {isPlannerWorking && (
              <Spinner style={{ minWidth: 16, minHeight: 16 }} />
            )}
            {!isPlannerWorking && isPlannerPaused && (
              <PauseIcon
                style={{
                  minWidth: 16,
                  minHeight: 16,
                  color: "var(--yellow-9)",
                }}
              />
            )}
            {!isPlannerWorking && !isPlannerPaused && isPlannerError && (
              <CrossCircledIcon
                style={{ minWidth: 16, minHeight: 16, color: "var(--red-9)" }}
              />
            )}
            {!isPlannerWorking &&
              !isPlannerPaused &&
              !isPlannerError &&
              isCompleted && (
                <CheckCircledIcon
                  style={{
                    minWidth: 16,
                    minHeight: 16,
                    color: "var(--green-9)",
                  }}
                />
              )}
            {!isPlannerWorking &&
              !isPlannerPaused &&
              !isPlannerError &&
              isFailed && (
                <CrossCircledIcon
                  style={{ minWidth: 16, minHeight: 16, color: "var(--red-9)" }}
                />
              )}
            {!isPlannerWorking &&
              !isPlannerPaused &&
              !isPlannerError &&
              !isCompleted &&
              !isFailed && (
                <DotFilledIcon
                  style={{
                    minWidth: 16,
                    minHeight: 16,
                    color: "var(--gray-9)",
                  }}
                />
              )}
            <Text
              as="div"
              size="2"
              weight="bold"
              style={{
                textOverflow: "ellipsis",
                overflow: "hidden",
                whiteSpace: "nowrap",
              }}
            >
              {task.name}
            </Text>
            <Badge color={statusColors[task.status]} size="1" ml="2">
              {statusLabels[task.status]}
            </Badge>
          </Flex>

          <Flex justify="between" mt="8px">
            <Flex gap="4">
              <Text
                size="1"
                style={{ display: "flex", gap: "4px", alignItems: "center" }}
              >
                <LayersIcon /> {task.cards_done}/{task.cards_total}
                {task.cards_failed > 0 && (
                  <Text size="1" color="red">
                    ({task.cards_failed} failed)
                  </Text>
                )}
              </Text>
              {task.agents_active > 0 && (
                <Text
                  size="1"
                  color="blue"
                  style={{ display: "flex", gap: "4px", alignItems: "center" }}
                >
                  <Spinner style={{ width: 12, height: 12 }} />{" "}
                  {task.agents_active} agent{task.agents_active > 1 ? "s" : ""}
                </Text>
              )}
            </Flex>
            <Text size="1" color="gray">
              {dateTimeString}
            </Text>
          </Flex>
        </button>
      </Card>

      <Flex
        position="absolute"
        top="6px"
        right="6px"
        gap="1"
        justify="end"
        align="center"
      >
        <CloseButton
          size="1"
          onClick={(event) => {
            event.preventDefault();
            event.stopPropagation();
            onDelete();
          }}
          iconSize={10}
          title="delete task"
        />
      </Flex>
    </Box>
=======

  if (isPlannerWorking)
    return { icon: LoaderCircle, tone: "accent" as const, spin: true };
  if (isPlannerPaused) return { icon: Pause, tone: "warning" as const };
  if (plannerState === "error")
    return { icon: CircleX, tone: "danger" as const };
  if (task.status === "completed")
    return { icon: CheckCircle, tone: "success" as const };
  if (task.status === "abandoned")
    return { icon: CircleX, tone: "danger" as const };
  return { icon: Circle, tone: "muted" as const };
}

const TaskItem: React.FC<TaskItemProps> = ({ task, onClick, onDelete }) => {
  const dateUpdated = new Date(task.updated_at);
  const dateTimeString = dateUpdated.toLocaleString();
  const statusIcon = taskStatusIcon(task, task.planner_session_state);

  return (
    <Card animated="rise" className={styles.taskItem} interactive>
      <button
        className={`${styles.taskItemButton} rf-pressable`}
        type="button"
        onClick={(event) => {
          event.preventDefault();
          event.stopPropagation();
          onClick();
        }}
      >
        <span className={styles.taskItemHeader}>
          <span className={styles.taskItemTitleGroup}>
            <span className={statusIcon.spin ? styles.taskSpinner : undefined}>
              <Icon icon={statusIcon.icon} size="md" tone={statusIcon.tone} />
            </span>
            <span className={styles.taskItemTitle}>{task.name}</span>
            <Badge tone={statusTones[task.status]}>
              {statusLabels[task.status]}
            </Badge>
          </span>
          <IconButton
            aria-label="delete task"
            icon={Trash2}
            size="sm"
            variant="ghost"
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              onDelete();
            }}
          />
        </span>

        <span className={styles.taskItemMetaRow}>
          <span className={styles.taskItemMetaGroup}>
            <span className={styles.taskItemMeta}>
              <Icon icon={Layers} size="sm" tone="muted" />
              {task.cards_done}/{task.cards_total}
              {task.cards_failed > 0 && (
                <span className={styles.taskItemDanger}>
                  ({task.cards_failed} failed)
                </span>
              )}
            </span>
            {task.agents_active > 0 && (
              <span className={styles.taskItemMetaAccent}>
                <span className={styles.taskSpinner}>
                  <Icon icon={LoaderCircle} size="sm" tone="accent" />
                </span>
                {task.agents_active} agent{task.agents_active > 1 ? "s" : ""}
              </span>
            )}
          </span>
          <span className={styles.taskItemDate}>{dateTimeString}</span>
        </span>
      </button>
    </Card>
>>>>>>> upstream/main
  );
};

interface TaskListProps {
  backFromTasks?: () => void;
}

export const TaskList: React.FC<TaskListProps> = ({ backFromTasks }) => {
  const dispatch = useAppDispatch();
  const { data: tasks = [], isLoading } = useListTasksQuery(undefined, {
    pollingInterval: 0,
  });
  const [createTask] = useCreateTaskMutation();
  const [deleteTask] = useDeleteTaskMutation();
  const [newTaskName, setNewTaskName] = useState("");
  const [newTaskTargetFiles, setNewTaskTargetFiles] = useState("");
  const [isCreating, setIsCreating] = useState(false);

  const handleBack = useCallback(() => {
    if (backFromTasks) {
      backFromTasks();
      return;
    }
    dispatch(pop());
  }, [backFromTasks, dispatch]);

  const handleCreateTask = useCallback(() => {
    if (!newTaskName.trim()) return;
    const targetFiles = newTaskTargetFiles
      .split(/[\n,]/)
      .map((file) => file.trim())
      .filter(Boolean);
    createTask({ name: newTaskName.trim(), target_files: targetFiles })
      .unwrap()
      .then((task) => {
        setNewTaskName("");
        setNewTaskTargetFiles("");
        setIsCreating(false);
        dispatch(openTask({ id: task.id, name: task.name }));
        dispatch(push({ name: "task workspace", taskId: task.id }));
      })
<<<<<<< HEAD
      .catch(() => {
        // Error handling via RTK Query
      });
=======
      .catch(() => undefined);
>>>>>>> upstream/main
  }, [createTask, dispatch, newTaskName, newTaskTargetFiles]);

  const handleTaskClick = useCallback(
    (task: TaskMeta) => {
      dispatch(openTask({ id: task.id, name: task.name }));
      dispatch(push({ name: "task workspace", taskId: task.id }));
    },
    [dispatch],
  );

  const handleDeleteTask = useCallback(
    (taskId: string) => {
      void deleteTask(taskId);
    },
    [deleteTask],
  );

  const handleKeyDown = useCallback(
<<<<<<< HEAD
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        handleCreateTask();
      } else if (e.key === "Escape") {
=======
    (event: React.KeyboardEvent) => {
      if (event.key === "Enter") {
        handleCreateTask();
      } else if (event.key === "Escape") {
>>>>>>> upstream/main
        setIsCreating(false);
        setNewTaskName("");
        setNewTaskTargetFiles("");
      }
    },
    [handleCreateTask],
  );

  if (isLoading) {
    return <ChatLoading />;
  }

  return (
<<<<<<< HEAD
    <Flex direction="column" style={{ height: "100%" }} p="4" gap="4">
      <Flex justify="between" align="center">
        <Flex align="center" gap="3">
          <Button
            variant="ghost"
            size="1"
            onClick={handleBack}
            aria-label="Back to previous page"
            title="Back"
          >
            <ArrowLeftIcon />
          </Button>
          <Heading size="4">Tasks</Heading>
        </Flex>
        {!isCreating && (
          <Button size="2" onClick={() => setIsCreating(true)}>
            <PlusIcon /> New Task
          </Button>
        )}
      </Flex>

      {isCreating && (
        <Card>
          <Flex direction="column" gap="2">
            <Flex gap="2">
              <TextField.Root
                style={{ flex: 1 }}
                placeholder="Task name..."
                value={newTaskName}
                onChange={(e) => setNewTaskName(e.target.value)}
                onKeyDown={handleKeyDown}
                autoFocus
              />
              <Button onClick={handleCreateTask} disabled={!newTaskName.trim()}>
                Create
              </Button>
              <Button
                variant="soft"
                color="gray"
                onClick={() => {
                  setIsCreating(false);
                  setNewTaskName("");
                  setNewTaskTargetFiles("");
                }}
              >
                Cancel
              </Button>
            </Flex>
            <TextArea
              aria-label="Target files"
              placeholder="Target files (comma or newline separated)"
              value={newTaskTargetFiles}
              onChange={(e) => setNewTaskTargetFiles(e.target.value)}
            />
          </Flex>
        </Card>
      )}

      <Box style={{ flex: 1, overflow: "hidden" }}>
        <ScrollArea scrollbars="vertical">
          <Flex direction="column" gap="2">
            {tasks.length === 0 ? (
              <Text color="gray" size="2">
                No tasks yet. Create one to start planning.
              </Text>
            ) : (
              tasks.map((task) => (
                <TaskItem
                  key={task.id}
                  task={task}
                  onClick={() => handleTaskClick(task)}
                  onDelete={() => handleDeleteTask(task.id)}
                />
              ))
            )}
          </Flex>
        </ScrollArea>
      </Box>
    </Flex>
=======
    <section className={styles.taskListRoot}>
      <header className={styles.taskListHeader}>
        <div className={styles.taskListTitleGroup}>
          <IconButton
            aria-label="Back to previous page"
            icon={ArrowLeft}
            size="sm"
            variant="ghost"
            title="Back"
            onClick={handleBack}
          />
          <h2 className={styles.taskListTitle}>Tasks</h2>
        </div>
        {!isCreating && (
          <Button
            leftIcon={Plus}
            size="sm"
            variant="soft"
            onClick={() => setIsCreating(true)}
          >
            New Task
          </Button>
        )}
      </header>

      {isCreating && (
        <Card animated="rise" className={styles.taskCreateCard}>
          <div className={styles.taskCreateRow}>
            <FieldText
              className={styles.taskCreateName}
              placeholder="Task name..."
              value={newTaskName}
              onChange={setNewTaskName}
              onKeyDown={handleKeyDown}
              autoFocus
            />
            <Button
              onClick={handleCreateTask}
              disabled={!newTaskName.trim()}
              variant="primary"
            >
              Create
            </Button>
            <Button
              variant="ghost"
              onClick={() => {
                setIsCreating(false);
                setNewTaskName("");
                setNewTaskTargetFiles("");
              }}
            >
              Cancel
            </Button>
          </div>
          <FieldTextarea
            aria-label="Target files"
            placeholder="Target files (comma or newline separated)"
            value={newTaskTargetFiles}
            onChange={setNewTaskTargetFiles}
          />
        </Card>
      )}

      <div className={styles.taskListScroller}>
        <div className={`${styles.taskListItems} rf-stagger`}>
          {tasks.length === 0 ? (
            <p className={styles.taskListEmpty}>
              No tasks yet. Create one to start planning.
            </p>
          ) : (
            tasks.map((task) => (
              <TaskItem
                key={task.id}
                task={task}
                onClick={() => handleTaskClick(task)}
                onDelete={() => handleDeleteTask(task.id)}
              />
            ))
          )}
        </div>
      </div>
    </section>
>>>>>>> upstream/main
  );
};
