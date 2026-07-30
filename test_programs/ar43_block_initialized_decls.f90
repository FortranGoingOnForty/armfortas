! A declaration initializer inside BLOCK implies SAVE. The initialized entity
! must therefore have module-owned storage that persists across entries, while
! same-named entities in distinct BLOCK scopes must remain independent.
!
! CHECK: 11 22 110 12 24 120
! IR_CHECK: _block_0_value: i32 = 100
! IR_CHECK: .block.0_value: i32 = 10
! IR_CHECK: .block.1_value: i32 = 20
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program ar43_block_initialized_decls
  implicit none
  integer :: first_a
  integer :: second_a
  integer :: owner_a
  integer :: first_b
  integer :: second_b
  integer :: owner_b

  call probe(first_a, second_a, owner_a)
  call probe(first_b, second_b, owner_b)

  print *, first_a, second_a, owner_a, first_b, second_b, owner_b
  if (first_a /= 11) error stop 1
  if (second_a /= 22) error stop 2
  if (owner_a /= 110) error stop 3
  if (first_b /= 12) error stop 4
  if (second_b /= 24) error stop 5
  if (owner_b /= 120) error stop 6
contains
  subroutine probe(first, second, owner)
    integer, intent(out) :: first
    integer, intent(out) :: second
    integer, intent(out) :: owner
    integer :: block_0_value = 100

    block_0_value = block_0_value + 10
    owner = block_0_value

    block
      integer :: value = 10
      value = value + 1
      first = value
    end block

    block
      integer :: value = 20
      value = value + 2
      second = value
    end block
  end subroutine probe
end program ar43_block_initialized_decls
