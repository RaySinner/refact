import React from "react";
import { Badge, Button, Card, Flex, Heading, Table } from "@radix-ui/themes";
import {
  useApproveBuddyArtifactMutation,
  useGetBuddyArtifactsQuery,
  useRejectBuddyArtifactMutation,
  type ArtifactStatus,
} from "../../services/refact/buddy";
import styles from "./ArtifactsPanel.module.css";

export const ArtifactsPanel: React.FC = () => {
  const { data, isLoading } = useGetBuddyArtifactsQuery(undefined);
  const [approve] = useApproveBuddyArtifactMutation();
  const [reject] = useRejectBuddyArtifactMutation();

  if (isLoading) return null;

  const ops = data?.ops ?? [];

  return (
    <Card className={styles.panel}>
      <Heading size="3" mb="2">
        📥 Memory Ops
      </Heading>
      <Table.Root>
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Title</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Type</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Status</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Created</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Actions</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {ops.map((op) => (
            <Table.Row key={op.op_id}>
              <Table.Cell>
                {op.title ?? op.payload?.title ?? op.op_id}
              </Table.Cell>
              <Table.Cell>{op.op_type}</Table.Cell>
              <Table.Cell>
                <Badge color={statusColor(op.status)}>{op.status}</Badge>
              </Table.Cell>
              <Table.Cell>{op.created_at}</Table.Cell>
              <Table.Cell>
                {isPending(op.status) && (
                  <Flex gap="2">
                    <Button
                      size="1"
                      onClick={() => void approve({ op_id: op.op_id })}
                    >
                      Approve
                    </Button>
                    <Button
                      size="1"
                      variant="soft"
                      color="red"
                      onClick={() => void reject({ op_id: op.op_id })}
                    >
                      Reject
                    </Button>
                  </Flex>
                )}
              </Table.Cell>
            </Table.Row>
          ))}
        </Table.Body>
      </Table.Root>
    </Card>
  );
};

function statusColor(
  status: ArtifactStatus,
): "gray" | "green" | "red" | "yellow" {
  const normalized = status.toLowerCase();
  if (normalized === "applied") return "green";
  if (normalized === "rejected" || normalized === "failed") return "red";
  if (normalized === "pending" || normalized === "approved") return "yellow";
  return "gray";
}

function isPending(status: ArtifactStatus): boolean {
  return status.toLowerCase() === "pending";
}
