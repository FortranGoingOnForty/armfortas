! CHECK: ok
! IR_CHECK: call @afs_open
! FILE_MISSING: fixed_name_trimmed.dat
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program open_fixed_char_filename_trim
  implicit none

  integer :: unit_num, ios, i
  logical :: exists_now
  character(len=256) :: filename
  character(len=512) :: msg

  filename = "fixed_name_trimmed.dat"

  do i = 1, 3
    ios = -1
    msg = "sentinel"

    open(newunit=unit_num, file=filename, status="replace", iostat=ios, iomsg=msg)
    if (ios /= 0) error stop 1

    write(unit_num, "(a)") "payload"

    inquire(file=filename, exist=exists_now)
    if (.not. exists_now) error stop 2

    close(unit_num, status="delete", iostat=ios, iomsg=msg)
    if (ios /= 0) error stop 3

    inquire(file=filename, exist=exists_now)
    if (exists_now) error stop 4
  end do

  write(*, "(a)") "ok"
end program open_fixed_char_filename_trim
