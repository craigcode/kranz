// Right column: open structured human questions (the pending-decision
// projection's second kind — ticket structured-human-question-events).
//
// A worker whose report carried a structured "ask the human" payload has its
// questions folded into `pendingQuestions`. This panel renders them in the
// SAME "your move" area as the parked grant (directly beside
// GrantRequestPanel, sharing its chrome classes) — distinct kinds, one
// decision stack, never a competing inbox. Answering an option button (or the
// free-text row) posts to the dedicated REST route, which enqueues the
// `answer-question` control command; the engine lands it as
// `question.answered` and the reducer routes it into the mission's
// user-message consult.

import { useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { PendingQuestion } from '../lib/types';

export function QuestionRequestPanel() {
  const state = useKranzStore((s) => s.state);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [freeText, setFreeText] = useState<Record<string, string>>({});

  const mission = state?.mission ?? null;
  const questions = state?.pendingQuestions ?? [];

  if (mission === null || questions.length === 0) return null;

  const answer = async (question: PendingQuestion, text: string, option?: number) => {
    setBusy(question.questionId);
    setError(null);
    try {
      await api.answerQuestion(mission.id, question.questionId, text, option);
      setFreeText((prev) => ({ ...prev, [question.questionId]: '' }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="panel panel-questions">
      <div className="section-label">
        Questions
        <span className="section-count mono">{questions.length}</span>
      </div>
      {questions.map((q) => (
        <div className="revision-body" key={q.questionId}>
          <div className="dim">
            {q.questionId}
            {q.featureId !== undefined ? ` · ${q.featureId}` : ''} — a worker asked:
          </div>
          <pre className="revision-diff">{q.text}</pre>
          {q.options !== undefined && q.options.length > 0 && (
            <div className="revision-actions">
              {q.options.map((option, index) => (
                <button
                  type="button"
                  className="strip-btn"
                  key={option}
                  disabled={busy !== null}
                  onClick={() => void answer(q, option, index)}
                >
                  {busy === q.questionId ? 'Answering...' : option}
                </button>
              ))}
            </div>
          )}
          <div className="revision-actions">
            <input
              type="text"
              className="revision-input"
              placeholder="Free-text answer..."
              value={freeText[q.questionId] ?? ''}
              disabled={busy !== null}
              onChange={(e) =>
                setFreeText((prev) => ({ ...prev, [q.questionId]: e.target.value }))
              }
            />
            <button
              type="button"
              className="strip-btn"
              disabled={busy !== null || (freeText[q.questionId] ?? '').trim() === ''}
              onClick={() => void answer(q, (freeText[q.questionId] ?? '').trim())}
            >
              Send
            </button>
          </div>
        </div>
      ))}
      {error !== null && (
        <div className="picker-error revision-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
