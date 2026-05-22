! CHECK: opened YES
! CHECK: write YES
! CHECK: sequential YES
! CHECK: formatted YES
! IR_CHECK: call @afs_inquire_unit
! FILE_MISSING: logger_inquire_probe.txt
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program inquire_positional_unit_capabilities
  implicit none

  integer :: unit
  integer :: ios
  logical :: opened
  character(12) :: write_spec
  character(12) :: sequential_spec
  character(12) :: formatted_spec

  open(newunit=unit, file="logger_inquire_probe.txt", form="formatted", &
       action="readwrite", status="replace", position="rewind", iostat=ios)
  if (ios /= 0) error stop 1

  inquire(unit, opened=opened, write=write_spec, sequential=sequential_spec, &
          formatted=formatted_spec, iostat=ios)
  if (ios /= 0) error stop 2
  if (.not. opened) error stop 3

  if (trim(write_spec) /= "YES") error stop 4
  if (trim(sequential_spec) /= "YES") error stop 5
  if (trim(formatted_spec) /= "YES") error stop 6

  close(unit, status="delete")

  write(*, '(a)') "opened YES"
  write(*, '(a,a)') "write ", trim(write_spec)
  write(*, '(a,a)') "sequential ", trim(sequential_spec)
  write(*, '(a,a)') "formatted ", trim(formatted_spec)
end program
