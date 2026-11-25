package daltoki

import (
	"context"
	"errors"
	"time"
)

const budgetDayLayout = "2006-01-02"

var (
	errDailyJobs   = errors.New("daltoki: daily job budget exhausted")
	errDailyTokens = errors.New("daltoki: daily token budget exhausted")
	errDailyCost   = errors.New("daltoki: daily cost budget exhausted")
)

type budgetRecord struct {
	JobID         string `json:"jobId"`
	Day           string `json:"day"`
	Tokens        int64  `json:"tokens"`
	CostUSDMicros int64  `json:"costUsdMicros"`
}

type BudgetLimits struct {
	JobsPerDay int
	Job        Usage
	Daily      Usage
}

type budgetVerdict int

const (
	budgetGrant budgetVerdict = iota
	budgetWait
	budgetRefuse
)

type dayLedger struct {
	jobs  int
	held  int
	spent Usage
}

func (s *serviceStore) checkBudget(record budgetRecord) error {
	job, known := s.jobs[record.JobID]
	_, reserved := s.budgets[record.JobID]
	_, dayErr := time.Parse(budgetDayLayout, record.Day)
	if !known || reserved || dayErr != nil || job.State != JobQueued || record.Tokens <= 0 || record.CostUSDMicros <= 0 {
		return errJournalDamaged
	}
	return nil
}

func holdsReservation(state JobState) bool {
	return state == JobQueued || state == JobRunning || state == JobUncertain
}

func (s *serviceStore) ledger(day string) dayLedger {
	var ledger dayLedger
	for jobID, reservation := range s.budgets {
		if reservation.Day != day {
			continue
		}
		ledger.jobs++
		job := s.jobs[jobID]
		if holdsReservation(job.State) {
			ledger.held++
			ledger.spent = ledger.spent.plus(Usage{Tokens: reservation.Tokens, CostUSDMicros: reservation.CostUSDMicros})
			continue
		}
		ledger.spent = ledger.spent.plus(job.Usage)
	}
	return ledger
}

func (l dayLedger) verdict(limits BudgetLimits) (budgetVerdict, error) {
	if l.jobs >= limits.JobsPerDay {
		return budgetRefuse, errDailyJobs
	}
	next := l.spent.plus(limits.Job)
	var exhausted error
	switch {
	case next.Tokens > limits.Daily.Tokens:
		exhausted = errDailyTokens
	case next.CostUSDMicros > limits.Daily.CostUSDMicros:
		exhausted = errDailyCost
	default:
		return budgetGrant, nil
	}
	if l.held > 0 {
		return budgetWait, exhausted
	}
	return budgetRefuse, exhausted
}

func (s *serviceStore) StartNext(ctx context.Context, limits BudgetLimits) (Job, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, id := range s.jobOrder {
		job := s.jobs[id]
		if job.State != JobQueued {
			continue
		}
		started, err := s.startJob(ctx, job, limits)
		if err != nil || started {
			return job.snapshot(), started, err
		}
		if job.State == JobQueued {
			return Job{}, false, nil
		}
	}
	return Job{}, false, nil
}

func (s *serviceStore) startJob(ctx context.Context, job *Job, limits BudgetLimits) (bool, error) {
	running := s.jobStateRecord(job.ID, JobRunning, "", nil)
	if _, reserved := s.budgets[job.ID]; reserved {
		if err := s.commit(ctx, running); err != nil {
			return false, err
		}
		return true, nil
	}
	now := s.clock().UTC()
	day := now.Format(budgetDayLayout)
	verdict, exhausted := s.ledger(day).verdict(limits)
	if verdict == budgetWait {
		return false, nil
	}
	if verdict == budgetRefuse {
		return false, s.commit(ctx, s.jobStateRecord(job.ID, JobBlocked, exhausted.Error(), nil))
	}
	reserve := s.record(kindBudgetReserved, now)
	reserve.Budget = &budgetRecord{JobID: job.ID, Day: day, Tokens: limits.Job.Tokens, CostUSDMicros: limits.Job.CostUSDMicros}
	if err := s.commit(ctx, reserve, running); err != nil {
		return false, err
	}
	return true, nil
}
