export type InlineActionFeedbackValue = {
  tone: "info" | "success" | "error";
  text: string;
};

type InlineActionFeedbackProps = {
  feedback?: InlineActionFeedbackValue | null;
  id?: string;
};

export function InlineActionFeedback({ feedback, id }: InlineActionFeedbackProps) {
  if (!feedback) return null;

  return (
    <p
      id={id}
      className="account-center-action-feedback"
      data-tone={feedback.tone}
      role={feedback.tone === "error" ? "alert" : "status"}
    >
      {feedback.text}
    </p>
  );
}
