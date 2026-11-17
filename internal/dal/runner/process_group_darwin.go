//go:build darwin

package runner

import (
	"golang.org/x/sys/unix"
)

const (
	darwinZombieStat = 5
	darwinSystemFlag = 0x200
)

func processGroupDrained(pgid int) bool {
	members, err := unix.SysctlKinfoProcSlice("kern.proc.pgrp", pgid)
	if err != nil {
		return false
	}
	for _, member := range members {
		if member.Proc.P_stat == darwinZombieStat {
			continue
		}
		if member.Proc.P_flag&darwinSystemFlag != 0 {
			continue
		}
		return false
	}
	return true
}
